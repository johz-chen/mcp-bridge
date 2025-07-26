use anyhow::{Result, anyhow};
use eventsource_client::{Client, ClientBuilder, ReconnectOptions, SSE};
use futures_util::StreamExt;
use reqwest::Client as HttpClient;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tracing::{debug, error, info, warn};
use tokio::time::{timeout, Duration};

#[derive(Debug)]
pub struct SseServer {
    url: String,
    http_client: HttpClient,
    event_loop_handle: Option<AbortHandle>,
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

        // 处理 SSE 客户端构建
        let sse_client = match timeout(Duration::from_secs(10), async {
            let builder = ClientBuilder::for_url(&sse_url)
                .map_err(|e| anyhow!("Invalid SSE URL: {:?}", e))?;
            Ok(builder
                .reconnect(
                    ReconnectOptions::reconnect(true)
                        .retry_initial(false)
                        .delay(Duration::from_secs(1))
                        .backoff_factor(2)
                        .delay_max(Duration::from_secs(30))
                        .build(),
                )
                .build())
        }).await {
            Ok(Ok(client)) => client,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(anyhow!("SSE connection timed out")),
        };

        let mut stream = sse_client.stream();

        // 创建可中止的任务
        let handle = tokio::spawn(async move {
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
                                        break;
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

        self.event_loop_handle = Some(handle.abort_handle());
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
            handle.abort();
        }

        self.is_running = false;
    }

    pub async fn send(&self, message: &str) -> Result<()> {
        debug!("Sending SSE call to {}", self.url);

        // 显式处理 JSON 解析错误
        let value = match serde_json::from_str::<Value>(message) {
            Ok(v) => v,
            Err(e) => return Err(anyhow!("Failed to parse message: {}", e)),
        };

        // 添加请求超时
        let response = timeout(Duration::from_secs(10), async {
            self.http_client
                .post(&self.url)
                .json(&value)
                .send()
                .await
        }).await??;

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

        let result = server.start().await;
        assert!(result.is_ok());
        assert!(server.is_running());

        server.stop().await;
        assert!(!server.is_running());
    }

    #[tokio::test]
    async fn test_sse_server_send_message() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/sse/call"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let (tx, _) = mpsc::channel(1);
        let server = SseServer::new(
            format!("{}/sse", mock_server.uri()),
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
