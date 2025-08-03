use super::core::Bridge;
use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::time::Instant;
use tracing::{debug, info, warn};

pub async fn handle_message(bridge: &mut Bridge, msg: Value) -> Result<()> {
    info!("<< Received message: {}", msg);

    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
        match method {
            "ping" => return handle_ping_request(bridge, &msg).await,
            "initialize" => return initialize(bridge, msg).await,
            "tools/list" => return handle_tools_list_request(bridge, msg).await,
            "tools/call" => return handle_tool_call(bridge, msg).await,
            "notifications/initialized" => {
                info!("Received notifications/initialized");
                return Ok(());
            }
            _ => {}
        }
    }

    warn!("Received unknown message type: {}", msg);
    Ok(())
}

async fn handle_ping_request(bridge: &mut Bridge, msg: &Value) -> Result<()> {
    if let Some(id) = msg.get("id").cloned() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        });

        bridge.broadcast_message(response.to_string()).await?;
        debug!("Sent pong response for ping");
    }

    bridge.last_activity = Instant::now();
    Ok(())
}

async fn initialize(bridge: &mut Bridge, msg: Value) -> Result<()> {
    info!("Handling initialize request");

    let id = msg["id"].clone();

    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {
                    "listChanged": false
                }
            },
            "serverInfo": {
                "name": "MCP Bridge",
                "version": "0.3.0"
            }
        }
    });

    bridge.broadcast_message(response.to_string()).await?;
    info!("Sent initialize response");

    bridge.initialized = true;
    info!("Bridge initialized");

    bridge.tools.clear();
    bridge.tool_service_map.clear();
    bridge.collected_servers.clear();
    bridge.tools_collected = false;
    bridge.tools_list_response_sent = false;

    for (server_name, stdin) in &mut bridge.processes_stdin {
        let tools_request = json!({
            "jsonrpc": "2.0",
            "id": format!("tools-list-{server_name}"),
            "method": "tools/list"
        });

        let message = tools_request.to_string() + "\n";
        stdin.write_all(message.as_bytes()).await?;
        debug!("Sent tools/list request to server: {server_name}");
    }

    Ok(())
}

async fn handle_tools_list_request(bridge: &mut Bridge, msg: Value) -> Result<()> {
    info!("Handling tools/list request");

    bridge.pending_tools_list_request = Some(msg);

    if bridge.tools_collected && !bridge.tools_list_response_sent {
        reply_tools_list(bridge).await?;
        bridge.tools_list_response_sent = true;
    }

    Ok(())
}

pub async fn reply_tools_list(bridge: &mut Bridge) -> Result<()> {
    if let Some(request) = bridge.pending_tools_list_request.take() {
        let id = request["id"].clone();

        let mut tools_list = Vec::new();
        for (prefixed_name, (_, tool_value)) in &bridge.tools {
            let mut tool = tool_value.clone();
            if let Some(obj) = tool.as_object_mut() {
                obj.insert("name".to_string(), Value::String(prefixed_name.clone()));
            }
            tools_list.push(tool);
        }

        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": tools_list,
                "nextCursor": ""
            }
        });

        bridge.broadcast_message(response.to_string()).await?;
        info!("Sent tools/list response with {} tools", tools_list.len());

        bridge.tools_list_response_sent = true;
    }

    Ok(())
}

async fn handle_tool_call(bridge: &mut Bridge, msg: Value) -> Result<()> {
    let id = msg["id"].clone();
    let method = msg["method"].as_str().unwrap_or("");
    let params = msg["params"].as_object();

    let prefixed_tool_name = params.and_then(|p| p["name"].as_str()).unwrap_or("");
    let arguments = params.and_then(|p| p["arguments"].as_object());

    info!("Handling tool call: {prefixed_tool_name} with id {id}");

    if let Some((server_name, original_tool_name)) =
        super::get_original_tool_name(bridge, prefixed_tool_name)
    {
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": {
                "name": original_tool_name,
                "arguments": arguments
            }
        });

        bridge
            .send_to_server(&server_name, &request.to_string())
            .await?;

        info!("Forwarded tool call to server: {server_name} (original: {original_tool_name})");
        return Ok(());
    }

    let error_response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": format!("Tool not found: {prefixed_tool_name}")
        }
    });

    bridge.broadcast_message(error_response.to_string()).await?;
    warn!("Tool not found: {prefixed_tool_name}");

    Ok(())
}

pub async fn notify_tools_changed(bridge: &mut Bridge) -> Result<()> {
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "workspace/toolsChanged",
        "params": {}
    });

    bridge.broadcast_message(notification.to_string()).await?;
    info!("Sent workspace/toolsChanged notification");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::core::Bridge;
    use crate::config::{AppConfig, BridgeConfig, ConnectionConfig, MqttConfig, WebSocketConfig};
    use anyhow::Result;
    use serde_json::json;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::process::Command;
    use tokio::sync::mpsc;

    // 创建测试桥接器
    fn create_test_bridge() -> Bridge {
        Bridge {
            config: BridgeConfig {
                app_config: AppConfig {
                    websocket: WebSocketConfig {
                        enabled: true,
                        endpoint: "wss://test.com".to_string(),
                    },
                    mqtt: MqttConfig {
                        enabled: false,
                        broker: "".to_string(),
                        port: 1883,
                        client_id: "test".to_string(),
                        topic: "".to_string(),
                    },
                    connection: ConnectionConfig::default(),
                },
                servers: HashMap::new(),
            },
            transports: vec![],
            processes_stdin: HashMap::new(),
            sse_servers: HashMap::new(),
            message_tx: mpsc::channel(100).0,
            message_rx: mpsc::channel(100).1,
            connection_config: Arc::new(ConnectionConfig::default()),
            is_connected: true,
            reconnect_attempt: 0,
            initialized: false,
            tools: HashMap::new(),
            tool_service_map: HashMap::new(),
            last_activity: Instant::now(),
            last_ping_sent: Instant::now(),
            pending_tools_list_request: None,
            tools_collected: false,
            collected_servers: HashSet::new(),
            tools_list_response_sent: false,
            active_servers: HashSet::new(),
        }
    }

    #[tokio::test]
    async fn test_handle_ping_request() -> Result<()> {
        let mut bridge = create_test_bridge();
        let ping_msg = json!({
            "jsonrpc": "2.0",
            "id": "ping-123",
            "method": "ping"
        });

        handle_message(&mut bridge, ping_msg).await?;

        // 验证最后活动时间已更新
        assert!(bridge.last_activity.elapsed() < Duration::from_millis(10));
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_initialize() -> Result<()> {
        let mut bridge = create_test_bridge();
        let init_msg = json!({
            "jsonrpc": "2.0",
            "id": "init-123",
            "method": "initialize"
        });

        handle_message(&mut bridge, init_msg).await?;

        // 验证桥接器已初始化
        assert!(bridge.initialized);
        // 验证工具列表已清空
        assert!(bridge.tools.is_empty());
        assert!(bridge.tool_service_map.is_empty());
        assert!(bridge.collected_servers.is_empty());
        assert!(!bridge.tools_collected);
        assert!(!bridge.tools_list_response_sent);

        Ok(())
    }

    #[tokio::test]
    async fn test_handle_tools_list_request() -> Result<()> {
        let mut bridge = create_test_bridge();
        let tools_list_msg = json!({
            "jsonrpc": "2.0",
            "id": "tools-list-123",
            "method": "tools/list"
        });

        // 第一次请求 - 工具尚未收集
        handle_message(&mut bridge, tools_list_msg.clone()).await?;
        assert!(bridge.pending_tools_list_request.is_some());

        // 添加一些工具
        bridge.tools.insert(
            "test_tool".to_string(),
            ("test_server".to_string(), json!({"name": "test_tool"})),
        );

        // 第二次请求 - 应该立即回复
        handle_message(&mut bridge, tools_list_msg).await?;
        assert!(bridge.pending_tools_list_request.is_none());
        assert!(bridge.tools_list_response_sent);

        Ok(())
    }

    #[tokio::test]
    async fn test_reply_tools_list() -> Result<()> {
        let mut bridge = create_test_bridge();

        // 添加测试工具
        bridge.tools.insert(
            "test_tool".to_string(),
            ("test_server".to_string(), json!({"name": "test_tool"})),
        );

        // 设置待处理的工具列表请求
        bridge.pending_tools_list_request = Some(json!({
            "jsonrpc": "2.0",
            "id": "tools-list-123",
            "method": "tools/list"
        }));

        reply_tools_list(&mut bridge).await?;

        // 验证请求已被处理
        assert!(bridge.pending_tools_list_request.is_none());
        assert!(bridge.tools_list_response_sent);
        Ok(())
    }

    #[tokio::test]
    async fn test_notify_tools_changed() -> Result<()> {
        let mut bridge = create_test_bridge();

        let result = notify_tools_changed(&mut bridge).await;
        assert!(result.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_tool_call_found() -> Result<()> {
        let mut bridge = create_test_bridge();

        // 添加测试工具映射
        bridge.tool_service_map.insert(
            "prefixed_tool".to_string(),
            ("test_server".to_string(), "original_tool".to_string()),
        );

        // 创建真实的子进程标准输入
        let mut dummy_process = Command::new("echo")
            .arg("hello")
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        let stdin = dummy_process.stdin.take().unwrap();
        bridge
            .processes_stdin
            .insert("test_server".to_string(), stdin);

        let tool_call_msg = json!({
            "jsonrpc": "2.0",
            "id": "call-123",
            "method": "tools/call",
            "params": {
                "name": "prefixed_tool",
                "arguments": {"param": "value"}
            }
        });

        handle_message(&mut bridge, tool_call_msg).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_handle_tool_call_not_found() -> Result<()> {
        let mut bridge = create_test_bridge();

        let tool_call_msg = json!({
            "jsonrpc": "2.0",
            "id": "call-123",
            "method": "tools/call",
            "params": {
                "name": "unknown_tool",
                "arguments": {}
            }
        });

        handle_message(&mut bridge, tool_call_msg).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_handle_unknown_message() -> Result<()> {
        let mut bridge = create_test_bridge();
        let unknown_msg = json!({
            "jsonrpc": "2.0",
            "method": "unknown.method"
        });

        handle_message(&mut bridge, unknown_msg).await?;

        Ok(())
    }
}
