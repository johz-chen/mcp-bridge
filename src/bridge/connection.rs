use super::core::Bridge;
use crate::bridge::message_handler::handle_message;
use anyhow::anyhow;
use serde_json::json;
use tokio::time::{Duration, Instant, sleep};
use tracing::{debug, error, info, warn};

pub async fn send_ping(bridge: &mut Bridge) -> anyhow::Result<()> {
    let time_since_last_activity = bridge.last_activity.elapsed();
    let time_since_last_ping = bridge.last_ping_sent.elapsed();

    if time_since_last_activity
        < Duration::from_millis(bridge.connection_config.heartbeat_interval / 3)
        || time_since_last_ping
            < Duration::from_millis(bridge.connection_config.heartbeat_interval / 2)
    {
        return Ok(());
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let ping_id = format!("ping-{timestamp}");

    let ping_message = json!({
        "jsonrpc": "2.0",
        "id": ping_id,
        "method": "ping"
    });

    bridge.broadcast_message(ping_message.to_string()).await?;
    bridge.last_ping_sent = Instant::now();
    debug!("Sent ping to keep connection alive");

    Ok(())
}

pub async fn reconnect(bridge: &mut Bridge) -> anyhow::Result<()> {
    if bridge.reconnect_attempt >= bridge.connection_config.max_reconnect_attempts {
        error!("Max reconnection attempts reached");
        return Err(anyhow!("Max reconnection attempts reached"));
    }

    bridge.reconnect_attempt += 1;
    let delay = Duration::from_millis(bridge.connection_config.reconnect_interval);

    warn!(
        "Attempting to reconnect (attempt {}) in {:?}",
        bridge.reconnect_attempt, delay
    );

    sleep(delay).await;

    for transport in &mut bridge.transports {
        let _ = transport.disconnect().await;
    }

    for transport in &mut bridge.transports {
        if let Err(e) = transport.connect().await {
            error!("Failed to reconnect transport: {}", e);
        }
    }

    bridge.is_connected = true;
    bridge.reconnect_attempt = 0;
    bridge.last_activity = Instant::now();
    bridge.last_ping_sent = Instant::now();

    info!("Successfully reconnected");

    bridge.initialized = false;
    bridge.tools_collected = false;
    bridge.tools_list_response_sent = false;
    bridge.collected_servers.clear();
    bridge.tools.clear();
    bridge.tool_service_map.clear();

    let init_msg = json!({
        "jsonrpc": "2.0",
        "id": "reinit-after-reconnect",
        "method": "initialize"
    });

    handle_message(bridge, init_msg).await?;

    Ok(())
}

pub async fn handle_transport_disconnect(bridge: &mut Bridge, index: usize) {
    if bridge.reconnect_attempt >= bridge.connection_config.max_reconnect_attempts {
        error!("Max reconnection attempts reached for transport {}", index);
        return;
    }

    bridge.reconnect_attempt += 1;
    let delay = Duration::from_millis(
        bridge.connection_config.reconnect_interval * (2u64.pow(bridge.reconnect_attempt)),
    );

    warn!(
        "Attempting to reconnect transport {} (attempt {}) in {:?}",
        index, bridge.reconnect_attempt, delay
    );

    tokio::time::sleep(delay).await;

    if let Err(e) = bridge.transports[index].connect().await {
        error!("Failed to reconnect transport {}: {}", index, e);
    } else {
        bridge.reconnect_attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::core::Bridge;
    use crate::config::{AppConfig, BridgeConfig, ConnectionConfig, MqttConfig, WebSocketConfig};
    use crate::transports::Transport;
    use async_trait::async_trait;
    use serde_json::Value;
    use std::any::Any;
    use std::collections::{HashMap, HashSet};
    use std::fmt;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::time::{Duration, Instant};

    fn create_test_bridge() -> Bridge {
        let (message_tx, _) = mpsc::channel(100);
        let (_, message_rx) = mpsc::channel(100);

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
                    connection: ConnectionConfig {
                        heartbeat_interval: 100,
                        heartbeat_timeout: 50,
                        reconnect_interval: 10,
                        max_reconnect_attempts: 3,
                    },
                },
                servers: HashMap::new(),
            },
            transports: vec![],
            processes_stdin: HashMap::new(),
            sse_servers: HashMap::new(),
            message_tx,
            message_rx,
            connection_config: Arc::new(ConnectionConfig {
                heartbeat_interval: 100,
                heartbeat_timeout: 50,
                reconnect_interval: 10,
                max_reconnect_attempts: 3,
            }),
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
    async fn test_send_ping() -> anyhow::Result<()> {
        let mut bridge = create_test_bridge();

        // 更新最后活动时间，确保满足发送条件
        bridge.last_activity = Instant::now() - Duration::from_millis(40);
        bridge.last_ping_sent = Instant::now() - Duration::from_millis(60);

        send_ping(&mut bridge).await?;

        // 验证最后发送时间已更新
        assert!(bridge.last_ping_sent.elapsed() < Duration::from_millis(10));
        Ok(())
    }

    #[tokio::test]
    async fn test_send_ping_not_needed() -> anyhow::Result<()> {
        let mut bridge = create_test_bridge();

        // 刚刚活动过，不应发送ping
        let result = send_ping(&mut bridge).await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_reconnect_success() -> anyhow::Result<()> {
        let mut bridge = create_test_bridge();
        bridge.is_connected = false;

        reconnect(&mut bridge).await?;

        // 验证重连状态
        assert!(bridge.is_connected);
        assert_eq!(bridge.reconnect_attempt, 0);
        assert!(bridge.last_activity.elapsed() < Duration::from_millis(10));
        assert!(bridge.last_ping_sent.elapsed() < Duration::from_millis(10));

        Ok(())
    }

    #[tokio::test]
    async fn test_reconnect_max_attempts() {
        let mut bridge = create_test_bridge();
        bridge.is_connected = false;
        bridge.reconnect_attempt = 3; // 达到最大尝试次数

        let result = reconnect(&mut bridge).await;
        assert!(result.is_err());
        assert_eq!(
            result.err().unwrap().to_string(),
            "Max reconnection attempts reached"
        );
    }

    #[tokio::test]
    async fn test_handle_transport_disconnect() {
        let mut bridge = create_test_bridge();
        bridge.transports = vec![
            Box::new(MockTransport::new(true)),
            Box::new(MockTransport::new(true)),
        ];

        // 模拟传输层断开
        handle_transport_disconnect(&mut bridge, 0).await;

        // 验证重连尝试次数已重置
        assert_eq!(bridge.reconnect_attempt, 0);
    }

    // 模拟传输层用于测试
    struct MockTransport {
        connected: bool,
    }

    impl MockTransport {
        fn new(connected: bool) -> Self {
            Self { connected }
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn connect(&mut self) -> anyhow::Result<()> {
            self.connected = true;
            Ok(())
        }

        async fn disconnect(&mut self) -> anyhow::Result<()> {
            self.connected = false;
            Ok(())
        }

        async fn send(&mut self, _msg: Value) -> anyhow::Result<()> {
            Ok(())
        }

        fn is_connected(&self) -> bool {
            self.connected
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    impl fmt::Debug for MockTransport {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "MockTransport")
        }
    }
}
