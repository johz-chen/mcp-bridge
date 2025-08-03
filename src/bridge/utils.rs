use super::core::Bridge;

pub fn generate_prefixed_tool_name(_bridge: &Bridge, server_name: &str, tool_name: &str) -> String {
    let normalized_server_name = server_name.replace('-', "_");
    format!("{normalized_server_name}_xzcli_{tool_name}")
}

pub fn get_original_tool_name(
    bridge: &Bridge,
    prefixed_tool_name: &str,
) -> Option<(String, String)> {
    bridge.tool_service_map.get(prefixed_tool_name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::core::Bridge;
    use crate::config::{AppConfig, BridgeConfig, ConnectionConfig, MqttConfig, WebSocketConfig};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

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
            sse_servers: HashMap::new(),
            processes_stdin: HashMap::new(),
            message_tx: tokio::sync::mpsc::channel(100).0,
            message_rx: tokio::sync::mpsc::channel(100).1,
            connection_config: Arc::new(ConnectionConfig::default()),
            is_connected: true,
            reconnect_attempt: 0,
            initialized: false,
            tools: HashMap::new(),
            tool_service_map: HashMap::new(),
            last_activity: tokio::time::Instant::now(),
            last_ping_sent: tokio::time::Instant::now(),
            pending_tools_list_request: None,
            tools_collected: false,
            tools_list_response_sent: false,
            collected_servers: std::collections::HashSet::new(),
            active_servers: HashSet::new(),
        }
    }

    #[test]
    fn test_generate_prefixed_tool_name() {
        let bridge = create_test_bridge();
        let name = generate_prefixed_tool_name(&bridge, "my-server", "my-tool");
        assert_eq!(name, "my_server_xzcli_my-tool");
    }

    #[test]
    fn test_get_original_tool_name_found() {
        let mut bridge = create_test_bridge();
        bridge.tool_service_map.insert(
            "prefixed_tool".to_string(),
            ("server1".to_string(), "original_tool".to_string()),
        );

        let result = get_original_tool_name(&bridge, "prefixed_tool");
        assert_eq!(
            result,
            Some(("server1".to_string(), "original_tool".to_string()))
        );
    }

    #[test]
    fn test_get_original_tool_name_not_found() {
        let bridge = create_test_bridge();
        let result = get_original_tool_name(&bridge, "unknown_tool");
        assert!(result.is_none());
    }
}
