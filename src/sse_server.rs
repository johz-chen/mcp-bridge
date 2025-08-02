use anyhow::{Result, anyhow};
use rmcp::{
    model::{ClientCapabilities, ClientInfo, Implementation, InitializeRequestParam, ClientRequest},
    service::RunningService,
    transport::sse_client::SseClientConfig,
    RoleClient, ServiceExt,
    transport::SseClientTransport,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info};
use reqwest::header::{HeaderName, HeaderValue, HeaderMap};
use serde_json::{Value, json}; 


#[derive(Debug)]
pub struct SseServer {
    url: String,
    headers: HashMap<String, String>,
    output_tx: mpsc::Sender<(String, String)>,
    server_name: String,
    is_running: bool,
    transport: Option<Arc<RunningService<RoleClient, InitializeRequestParam>>>,
    service_task: Option<tokio::task::JoinHandle<()>>,
}

impl SseServer {
    pub fn new(
        url: String,
        headers: HashMap<String, String>,
        output_tx: mpsc::Sender<(String, String)>,
        server_name: String,
    ) -> Self {
        Self {
            url,
            headers,
            output_tx,
            server_name,
            is_running: false,
            transport: None,
            service_task: None,
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        if self.is_running {
            return Ok(());
        }

        info!("Starting SSE server: {}", self.server_name);

        // 构建自定义 reqwest client
        let mut headers = HeaderMap::new();
        for (key, value) in &self.headers {
            headers.insert(
                HeaderName::from_bytes(key.as_bytes())?,
                HeaderValue::from_str(value)?,
            );
        }

        let rclient = reqwest::ClientBuilder::new()
            .default_headers(headers)
            .build()?;

        let transport = SseClientTransport::start_with_client(
            rclient,
            SseClientConfig {
                sse_endpoint: self.url.clone().into(),
                ..Default::default()
            },
        )
        .await?;

        let client_info = ClientInfo {
            protocol_version: Default::default(),
            capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: "mcp-bridge".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let client = client_info.serve(transport).await.inspect_err(|e| {
            tracing::error!("client error: {:?}", e);
        })?;

        // Initialize
        let server_info = client.peer_info();
        tracing::info!("Connected to server: {server_info:#?}");

        // List tools
        let tools = client.list_tools(Default::default()).await?;
        info!("Collected {} tools from SSE server {}", tools.tools.len(), self.server_name);

        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": format!("tools-list-{}", self.server_name),
            "result": {
                "tools": tools.tools
            }
        });

        if let Err(e) = self.output_tx.send((self.server_name.clone(), response.to_string())).await {
            error!("Failed to send tools list to bridge: {}", e);
        }

        self.transport = Some(Arc::new(client));

        Ok(())
    }

    pub async fn stop(&mut self) {
        if self.is_running {
            if let Some(task) = self.service_task.take() {
                task.abort();
            }
            self.transport = None;
            self.is_running = false;
        }
    }

    pub fn is_running(&self) -> bool {
        self.is_running
    }

pub async fn send(&self, message: &str) -> Result<String> {
    debug!("Sending message to SSE server: {}", message);

    let client = self.transport.as_ref()
        .ok_or_else(|| anyhow!("SSE client not initialized"))?;

    let request_value: Value = serde_json::from_str(message)
        .map_err(|e| anyhow!("Invalid JSON: {}", e))?;
    let original_id = request_value.get("id").cloned().unwrap_or(Value::Null);

    let rpc_req: ClientRequest = serde_json::from_str(message)
        .map_err(|e| anyhow!("Invalid JSON-RPC: {}", e))?;

    let resp = client.send_request(rpc_req).await?;
    let mut resp_value = serde_json::to_value(&resp)?;

    let final_response = if resp_value.get("result").is_some() || resp_value.get("error").is_some() {
        if resp_value.get("id").is_none() {
            resp_value["id"] = original_id.clone();
        }
        resp_value
    } else {
        json!({
            "id": original_id,
            "jsonrpc": "2.0",
            "result": resp_value
        })
    };

    let resp_str = serde_json::to_string(&final_response)?;

    self.output_tx
        .send((self.server_name.clone(), resp_str.clone()))
        .await
        .map_err(|e| anyhow!("Failed to send SSE response to bridge: {}", e))?;

    Ok(resp_str)
}
}


#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_sse_server_creation() {
        let (tx, _) = mpsc::channel(1);
        let headers = HashMap::new();
        let server = SseServer::new(
            "http://localhost:8080/sse".to_string(),
            headers,
            tx,
            "test_server".to_string(),
        );

        assert_eq!(server.url, "http://localhost:8080/sse");
        assert_eq!(server.server_name, "test_server");
        assert!(!server.is_running());
    }
}
