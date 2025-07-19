use super::*;
use crate::config::{BridgeConfig, ConnectionConfig};
use crate::process::ManagedProcess;
use crate::transports::{MqttTransport, Transport, WebSocketTransport};
use anyhow::{Context, anyhow};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::process::ChildStdin;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, interval};
use tracing::{debug, error, info, warn};

pub struct Bridge {
    pub config: BridgeConfig,
    pub transports: Vec<Box<dyn Transport>>,
    pub processes_stdin: HashMap<String, ChildStdin>,
    #[allow(dead_code)]
    pub message_tx: mpsc::Sender<Value>,
    pub message_rx: mpsc::Receiver<Value>,
    pub connection_config: Arc<ConnectionConfig>,
    pub is_connected: bool,
    pub reconnect_attempt: u32,
    pub initialized: bool,
    pub tools: HashMap<String, (String, Value)>,
    pub tool_service_map: HashMap<String, (String, String)>,
    pub last_activity: Instant,
    pub last_ping_sent: Instant,
    pub pending_tools_list_request: Option<Value>,
    pub tools_collected: bool,
    pub collected_servers: HashSet<String>,
}

impl Bridge {
    pub async fn new(config: BridgeConfig) -> anyhow::Result<Self> {
        let connection_config = Arc::new(config.app_config.connection.clone());
        let (message_tx, message_rx) = mpsc::channel(100);

        let mut transports: Vec<Box<dyn Transport>> = Vec::new();

        // 初始化 WebSocket 传输
        if config.app_config.websocket.enabled {
            let ws = WebSocketTransport::new(
                config.app_config.websocket.endpoint.clone(),
                message_tx.clone(),
            )
            .await
            .with_context(|| "Failed to initialize WebSocket transport")?;
            transports.push(Box::new(ws));
        }

        // 初始化 MQTT 传输
        if config.app_config.mqtt.enabled {
            let mqtt = MqttTransport::new(config.app_config.mqtt.clone(), message_tx.clone())
                .await
                .context("Failed to initialize MQTT transport")?;
            transports.push(Box::new(mqtt));
        }

        // 确保至少有一个传输层启用
        if transports.is_empty() {
            return Err(anyhow!("No transports enabled in configuration"));
        }

        let now = Instant::now();
        Ok(Self {
            config,
            transports,
            processes_stdin: HashMap::new(),
            message_tx,
            message_rx,
            connection_config,
            is_connected: false,
            reconnect_attempt: 0,
            initialized: false,
            tools: HashMap::new(),
            tool_service_map: HashMap::new(),
            last_activity: now,
            last_ping_sent: now,
            pending_tools_list_request: None,
            tools_collected: false,
            collected_servers: HashSet::new(),
        })
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        info!("MCP Bridge started");

        let (process_output_tx, mut process_output_rx) = mpsc::channel(100);

        let mut processes = HashMap::new();
        for (server_name, process_config) in self.config.servers.clone() {
            let mut process = ManagedProcess::new(&process_config)?;
            process
                .start(process_output_tx.clone(), server_name.clone())
                .await?;
            info!("Started server process: {}", server_name);

            if let Some(stdin) = process.stdin.take() {
                self.processes_stdin.insert(server_name.clone(), stdin);
            }

            processes.insert(server_name, process);
        }

        for transport in &mut self.transports {
            transport.connect().await?;
        }
        self.is_connected = true;
        self.last_activity = Instant::now();

        let heartbeat_interval = self.connection_config.heartbeat_interval;
        let heartbeat_timeout = self.connection_config.heartbeat_timeout;
        let ping_interval = heartbeat_interval / 2;

        let mut ping_interval_timer = interval(Duration::from_millis(ping_interval));

        loop {
            let timeout = tokio::time::sleep(Duration::from_millis(heartbeat_interval));
            tokio::pin!(timeout);

            tokio::select! {
                Some(msg) = self.message_rx.recv() => {
                    self.last_activity = Instant::now();
                    handle_message(&mut self, msg).await?;
                }
                Some((server_name, output)) = process_output_rx.recv() => {
                    self.last_activity = Instant::now();
                    handle_process_output(&mut self, &server_name, output).await?;
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("Shutting down...");
                    break;
                }
                _ = ping_interval_timer.tick() => {
                    send_ping(&mut self).await?;
                }
                _ = &mut timeout => {
                    let elapsed = self.last_activity.elapsed();
                    if elapsed > Duration::from_millis(heartbeat_timeout) {
                        warn!("Connection timeout detected ({}ms > {}ms)",
                              elapsed.as_millis(), heartbeat_timeout);
                        reconnect(&mut self).await?;
                    }
                }
            }
        }

        for (_, mut process) in processes {
            process.stop().await?;
        }

        self.shutdown().await?;
        Ok(())
    }

    pub async fn broadcast_message(&mut self, msg: String) -> anyhow::Result<()> {
        let trimmed = msg.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        debug!(">> Sending message: {}", trimmed);

        let msg_value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => Value::String(trimmed.to_string()),
        };

        let mut failed_transports = Vec::new();

        for (i, transport) in self.transports.iter_mut().enumerate() {
            if transport.is_connected() {
                if let Err(e) = transport.send(msg_value.clone()).await {
                    error!("Failed to send message via transport: {}", e);
                    failed_transports.push(i);
                }
            }
        }

        for i in failed_transports {
            handle_transport_disconnect(self, i).await;
        }

        self.last_activity = Instant::now();
        Ok(())
    }

    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        info!("Shutting down MCP Bridge");

        for transport in &mut self.transports {
            transport.disconnect().await?;
        }

        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, WebSocketConfig, MqttConfig, ConnectionConfig};
    use std::collections::HashMap;
    use async_trait::async_trait;
    use std::any::Any;
    use serde_json::Value;

    // 模拟传输层实现
    #[derive(Debug)]
    struct MockTransport;

    #[async_trait]
    impl Transport for MockTransport {
        async fn connect(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        async fn send(&mut self, _msg: Value) -> anyhow::Result<()> {
            Ok(())
        }

        fn is_connected(&self) -> bool {
            true
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    // 测试工具函数
    fn create_test_config() -> BridgeConfig {
        BridgeConfig {
            app_config: AppConfig {
                websocket: WebSocketConfig {
                    enabled: false,  // 禁用真实WebSocket
                    endpoint: "".to_string(),
                },
                mqtt: MqttConfig {
                    enabled: false,  // 禁用真实MQTT
                    broker: "".to_string(),
                    port: 1883,
                    client_id: "".to_string(),
                    topic: "".to_string(),
                },
                connection: ConnectionConfig::default(),
            },
            servers: HashMap::new(),
        }
    }

    // 修改 Bridge::new 以允许注入模拟传输层
    impl Bridge {
        pub async fn new_with_transports(config: BridgeConfig, transports: Vec<Box<dyn Transport>>) -> anyhow::Result<Self> {
            let connection_config = Arc::new(config.app_config.connection.clone());
            let (message_tx, message_rx) = mpsc::channel(100);

            // 确保至少有一个传输层
            if transports.is_empty() {
                return Err(anyhow!("No transports provided"));
            }

            let now = Instant::now();
            Ok(Self {
                config,
                transports,
                processes_stdin: HashMap::new(),
                message_tx,
                message_rx,
                connection_config,
                is_connected: false,
                reconnect_attempt: 0,
                initialized: false,
                tools: HashMap::new(),
                tool_service_map: HashMap::new(),
                last_activity: now,
                last_ping_sent: now,
                pending_tools_list_request: None,
                tools_collected: false,
                collected_servers: HashSet::new(),
            })
        }
    }

    #[tokio::test]
    async fn test_bridge_creation() {
        let config = create_test_config();
        let mock_transport = Box::new(MockTransport);
        let bridge = Bridge::new_with_transports(config, vec![mock_transport]).await;
        assert!(bridge.is_ok());
    }

    #[tokio::test]
    async fn test_bridge_without_transports() {
        let config = create_test_config();
        let bridge = Bridge::new_with_transports(config, vec![]).await;
        assert!(bridge.is_err());
        assert_eq!(
            bridge.err().unwrap().to_string(),
            "No transports provided"
        );
    }

    #[tokio::test]
    async fn test_broadcast_message() {
        let config = create_test_config();
        let mock_transport = Box::new(MockTransport);
        let mut bridge = Bridge::new_with_transports(config, vec![mock_transport]).await.unwrap();
        
        let result = bridge.broadcast_message(r#"{"test": "message"}"#.to_string()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_broadcast_empty_message() {
        let config = create_test_config();
        let mock_transport = Box::new(MockTransport);
        let mut bridge = Bridge::new_with_transports(config, vec![mock_transport]).await.unwrap();
        
        let result = bridge.broadcast_message("   ".to_string()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_broadcast_invalid_json() {
        let config = create_test_config();
        let mock_transport = Box::new(MockTransport);
        let mut bridge = Bridge::new_with_transports(config, vec![mock_transport]).await.unwrap();
        
        let result = bridge.broadcast_message("invalid json".to_string()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_shutdown() {
        let config = create_test_config();
        let mock_transport = Box::new(MockTransport);
        let mut bridge = Bridge::new_with_transports(config, vec![mock_transport]).await.unwrap();
        
        let result = bridge.shutdown().await;
        assert!(result.is_ok());
    }
}
