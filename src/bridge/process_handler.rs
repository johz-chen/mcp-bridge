use super::core::Bridge;
use anyhow::Result;
use serde_json::Value;
use tokio::time::Instant;
use tracing::{debug, info};

pub async fn handle_process_output(
    bridge: &mut Bridge,
    server_name: &str,
    output: String,
) -> Result<()> {
    debug!(">> Received output from {server_name}: {}", output.trim());

    let msg_value: Value = match serde_json::from_str(&output) {
        Ok(value) => value,
        Err(_) => Value::String(output.clone()),
    };

    let mut should_broadcast = true;

    if let Some(result) = msg_value.get("result") {
        if let Some(id) = msg_value.get("id") {
            if msg_value.get("method").is_none() {
                if let Some(id_str) = id.as_str() {
                    if id_str.starts_with("tools-list-") {
                        should_broadcast = false;

                        if let Some(tools) = result["tools"].as_array() {
                            bridge.collected_servers.insert(server_name.to_string());

                            for tool in tools {
                                if let Some(tool_obj) = tool.as_object() {
                                    if let Some(Value::String(original_tool_name)) =
                                        tool_obj.get("name")
                                    {
                                        let prefixed_name = super::generate_prefixed_tool_name(
                                            bridge,
                                            server_name,
                                            original_tool_name,
                                        );

                                        bridge.tools.insert(
                                            prefixed_name.clone(),
                                            (server_name.to_string(), tool.clone()),
                                        );

                                        bridge.tool_service_map.insert(
                                            prefixed_name,
                                            (server_name.to_string(), original_tool_name.clone()),
                                        );
                                    }
                                }
                            }

                            info!("Collected {} tools from {server_name}", tools.len());

                            if bridge.collected_servers.len() == bridge.config.servers.len() {
                                bridge.tools_collected = true;
                                info!("All tools collected, total: {}", bridge.tools.len());

                                super::message_handler::reply_tools_list(bridge).await?;
                            }
                        }
                    }
                }
            }
        }
    }

    if should_broadcast {
        bridge.broadcast_message(msg_value.to_string()).await?;
    }

    bridge.last_activity = Instant::now();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::core::Bridge;
    use crate::config::{
        AppConfig, BridgeConfig, ConnectionConfig, MqttConfig, ProcessConfig, WebSocketConfig,
    };
    use serde_json::json;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio::time::Instant;

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
        }
    }

    #[tokio::test]
    async fn test_handle_normal_output() -> anyhow::Result<()> {
        let mut bridge = create_test_bridge();
        let output = r#"{"jsonrpc": "2.0", "result": "test"}"#.to_string();

        let result = handle_process_output(&mut bridge, "test_server", output).await;
        assert!(result.is_ok());

        // 验证最后活动时间已更新
        assert!(bridge.last_activity.elapsed() < Duration::from_millis(10));
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_tools_list_output() -> anyhow::Result<()> {
        let mut bridge = create_test_bridge();

        // 创建工具列表响应
        let output = json!({
            "jsonrpc": "2.0",
            "id": "tools-list-test_server",
            "result": {
                "tools": [
                    {"name": "tool1"},
                    {"name": "tool2"}
                ]
            }
        })
        .to_string();

        let result = handle_process_output(&mut bridge, "test_server", output).await;
        assert!(result.is_ok());

        // 验证工具已收集
        assert!(bridge.collected_servers.contains("test_server"));
        assert_eq!(bridge.tools.len(), 2);
        assert!(bridge.tools.contains_key("test_server_xzcli_tool1"));
        assert!(bridge.tools.contains_key("test_server_xzcli_tool2"));

        // 验证服务映射
        assert_eq!(
            bridge.tool_service_map.get("test_server_xzcli_tool1"),
            Some(&("test_server".to_string(), "tool1".to_string()))
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_handle_all_servers_collected() -> anyhow::Result<()> {
        let mut bridge = create_test_bridge();

        // 设置服务器配置
        bridge.config.servers.insert(
            "server1".to_string(),
            ProcessConfig {
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
            },
        );
        bridge.config.servers.insert(
            "server2".to_string(),
            ProcessConfig {
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
            },
        );

        // 设置待处理的工具列表请求
        bridge.pending_tools_list_request = Some(json!({
            "jsonrpc": "2.0",
            "id": "tools-list-request",
            "method": "tools/list"
        }));

        // 创建第一个服务器的工具列表响应
        let output1 = json!({
            "jsonrpc": "2.0",
            "id": "tools-list-server1",
            "result": {
                "tools": [{"name": "tool1"}]
            }
        })
        .to_string();

        handle_process_output(&mut bridge, "server1", output1).await?;

        // 验证尚未全部收集完成
        assert!(!bridge.tools_collected);

        // 创建第二个服务器的工具列表响应
        let output2 = json!({
            "jsonrpc": "2.0",
            "id": "tools-list-server2",
            "result": {
                "tools": [{"name": "tool2"}]
            }
        })
        .to_string();

        handle_process_output(&mut bridge, "server2", output2).await?;

        // 验证全部收集完成
        assert!(bridge.tools_collected);
        assert_eq!(bridge.tools.len(), 2);

        Ok(())
    }

    #[tokio::test]
    async fn test_handle_invalid_tools_list_output() -> anyhow::Result<()> {
        let mut bridge = create_test_bridge();

        // 无效的工具列表响应
        let output = json!({
            "jsonrpc": "2.0",
            "id": "tools-list-test_server",
            "result": "invalid" // 不是数组
        })
        .to_string();

        let result = handle_process_output(&mut bridge, "test_server", output).await;
        assert!(result.is_ok());

        // 验证没有工具被收集
        assert!(bridge.tools.is_empty());

        Ok(())
    }
}
