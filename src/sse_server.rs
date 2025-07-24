use anyhow::{Result, anyhow};
use eventsource_client::{Client, ClientBuilder, ReconnectOptions, SSE};
use futures_util::StreamExt;
use reqwest::Client as HttpClient;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

#[derive(Debug)]
pub struct SseServer {
    url: String,
    http_client: HttpClient,
    event_loop_handle: Option<JoinHandle<()>>,
    output_tx: mpsc::Sender<(String, String)>,
    server_name: String,
    shutdown_tx: Option<mpsc::Sender<()>>,
    is_running: bool,
}

impl SseServer {
    pub fn new(
        url: String,
        output_tx: mpsc::Sender<(String, String)>,
        server_name: String,
    ) -> Self {
        Self {
            url,
            http_client: HttpClient::new(),
            event_loop_handle: None,
            output_tx,
            server_name,
            shutdown_tx: None,
            is_running: false,
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        if self.is_running {
            return Ok(());
        }

        info!("Starting SSE server: {}", self.url);

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let event_tx = self.output_tx.clone();
        let server_name = self.server_name.clone();
        let sse_url = self.url.clone();

        // 创建 SSE 客户端
        let mut builder =
            ClientBuilder::for_url(&sse_url).map_err(|e| anyhow!("Invalid SSE URL: {:?}", e))?;

        builder = builder.reconnect(
            ReconnectOptions::reconnect(true)
                .retry_initial(false)
                .delay(std::time::Duration::from_secs(1))
                .backoff_factor(2)
                .delay_max(std::time::Duration::from_secs(30))
                .build(),
        );

        let sse_client = builder.build();

        let handle = tokio::spawn(async move {
            let mut stream = sse_client.stream();
            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        debug!("SSE server received shutdown signal");
                        break;
                    }
                    event = stream.next() => {
                        match event {
                            Some(Ok(SSE::Event(event))) => {
                                if !event.data.is_empty() {
                                    debug!("Received SSE event (type: {}): {}", event.event_type, event.data);
                                    if let Err(e) = event_tx.send((server_name.clone(), event.data.clone())).await {
                                        error!("Failed to send SSE message: {}", e);
                                    }
                                } else {
                                    debug!("Received empty SSE event (type: {})", event.event_type);
                                }
                            }
                            Some(Ok(SSE::Comment(comment))) => {
                                debug!("Received SSE comment: {}", comment);
                            }
                            Some(Err(e)) => {
                                warn!("SSE error: {:?}", e);
                                break;
                            }
                            None => {
                                debug!("SSE stream ended");
                                break;
                            }
                        }
                    }
                }
            }
            debug!("SSE server exiting");
        });

        self.event_loop_handle = Some(handle);
        self.shutdown_tx = Some(shutdown_tx);
        self.is_running = true;

        Ok(())
    }

    pub async fn stop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            if let Err(e) = shutdown_tx.send(()).await {
                error!("Failed to send shutdown signal to SSE server: {}", e);
            }
        }

        if let Some(handle) = self.event_loop_handle.take() {
            if let Err(e) = handle.await {
                error!("Error joining SSE server task: {}", e);
            }
        }

        self.is_running = false;
    }

    pub async fn send(&self, message: &str) -> Result<()> {
        // 使用相同的 URL 作为调用端点
        let call_url = format!("{}/call", self.url.trim_end_matches('/'));
        debug!("Sending SSE call to {}", call_url);

        let response = self
            .http_client
            .post(&call_url)
            .json(&serde_json::from_str::<Value>(message)?)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("SSE call failed with status {}: {}", status, body));
        }

        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.is_running
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_sse_server_creation() {
        let (tx, _) = mpsc::channel(1);
        let server = SseServer::new(
            "http://localhost:8080/sse".to_string(),
            tx,
            "test_server".to_string(),
        );

        assert_eq!(server.url, "http://localhost:8080/sse");
        assert_eq!(server.server_name, "test_server");
        assert!(!server.is_running());
    }

    #[tokio::test]
    async fn test_sse_server_start_stop() {
        let (tx, _rx) = mpsc::channel(10);
        let mut server = SseServer::new(
            "http://localhost:8080/sse".to_string(),
            tx,
            "test_server".to_string(),
        );

        // 启动
        let result = server.start().await;
        assert!(result.is_ok());
        assert!(server.is_running());

        // 停止
        server.stop().await;
        assert!(!server.is_running());
    }

    #[tokio::test]
    async fn test_sse_server_send_message() {
        let mock_server = MockServer::start().await;

        // 设置 mock 响应 - 注意 URL 格式
        Mock::given(method("POST"))
            .and(path("/sse/call")) // 修改为 /sse/call 而不是 /call
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let (tx, _) = mpsc::channel(1);
        let server = SseServer::new(
            format!("{}/sse", mock_server.uri()), // 基础 URL 是 /sse
            tx,
            "test_server".to_string(),
        );

        let message = r#"{"jsonrpc": "2.0", "method": "test", "params": {}}"#;
        let result = server.send(message).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_sse_server_send_message_failure() {
        let mock_server = MockServer::start().await;

        // 设置 mock 失败响应 - 同样修改路径
        Mock::given(method("POST"))
            .and(path("/sse/call"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let (tx, _) = mpsc::channel(1);
        let server = SseServer::new(
            format!("{}/sse", mock_server.uri()),
            tx,
            "test_server".to_string(),
        );

        let message = r#"{"jsonrpc": "2.0", "method": "test"}"#;
        let result = server.send(message).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_sse_server_start_twice() {
        let (tx, _) = mpsc::channel(1);
        let mut server = SseServer::new(
            "http://localhost:8080/sse".to_string(),
            tx,
            "test_server".to_string(),
        );

        let result1 = server.start().await;
        let result2 = server.start().await;

        assert!(result1.is_ok());
        assert!(result2.is_ok());
        server.stop().await;
    }
}
