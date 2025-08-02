use super::*;
use crate::bridge::message_handler::{notify_tools_changed, reply_tools_list};
use crate::config::{BridgeConfig, ConnectionConfig, ServerConfig};
use crate::process::ManagedProcess;
use crate::sse_server::SseServer;
use crate::transports::{MqttTransport, Transport, WebSocketTransport};
use anyhow::{Context, anyhow};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::signal;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, interval};
use tracing::{debug, error, info, warn};

pub struct Bridge {
    pub config: BridgeConfig,
    pub transports: Vec<Box<dyn Transport>>,
    pub processes_stdin: HashMap<String, ChildStdin>,
    pub sse_servers: HashMap<String, SseServer>,
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
    pub tools_list_response_sent: bool,
    pub active_servers: HashSet<String>,
}

impl Bridge {
    pub async fn new(config: BridgeConfig) -> anyhow::Result<Self> {
        let connection_config = Arc::new(config.app_config.connection.clone());
        let (message_tx, message_rx) = mpsc::channel(100);

        let mut transports: Vec<Box<dyn Transport>> = Vec::new();

        if config.app_config.websocket.enabled {
            let ws = WebSocketTransport::new(
                config.app_config.websocket.endpoint.clone(),
                message_tx.clone(),
            )
            .await
            .with_context(|| "Failed to initialize WebSocket transport")?;
            transports.push(Box::new(ws));
        }

        if config.app_config.mqtt.enabled {
            let mqtt = MqttTransport::new(config.app_config.mqtt.clone(), message_tx.clone())
                .await
                .context("Failed to initialize MQTT transport")?;
            transports.push(Box::new(mqtt));
        }

        if transports.is_empty() {
            return Err(anyhow!("No transports enabled in configuration"));
        }

        let now = Instant::now();
        Ok(Self {
            config,
            transports,
            processes_stdin: HashMap::new(),
            sse_servers: HashMap::new(),
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
            tools_list_response_sent: false,
            active_servers: HashSet::new(),
        })
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        info!("MCP Bridge started");

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        tokio::spawn(async move {
            if let Err(e) = signal::ctrl_c().await {
                eprintln!("Failed to listen for ctrl_c signal: {e}");
            } else {
                shutdown_tx.send(()).await.ok();
            }
        });

        let (process_output_tx, mut process_output_rx) = mpsc::channel(100);
        for (server_name, server_config) in self.config.servers.clone() {
            match server_config {
                ServerConfig::Std { command, args, env } => {
                    let config = ServerConfig::Std { command, args, env };
                    match ManagedProcess::new(&config) {
                        Ok(mut process) => {
                            if let Err(e) = process
                                .start(process_output_tx.clone(), server_name.clone())
                                .await
                            {
                                warn!("Failed to start process server {}: {}", server_name, e);
                                continue;
                            }
                            info!("Started process server: {}", server_name);

                            if let Some(stdin) = process.stdin.take() {
                                self.processes_stdin.insert(server_name.clone(), stdin);
                            }
                            self.active_servers.insert(server_name.clone());
                        }
                        Err(e) => {
                            warn!("Failed to create process for server {}: {}", server_name, e);
                        }
                    }
                }
                ServerConfig::Sse { url, headers } => {
                    let mut sse_server = SseServer::new(
                        url,
                        headers,
                        process_output_tx.clone(),
                        server_name.clone(),
                    );
                    if let Err(e) = sse_server.start().await {
                        warn!("Failed to start SSE server {}: {}", server_name, e);
                    } else {
                        info!("Started SSE server: {}", server_name);
                        self.sse_servers.insert(server_name.clone(), sse_server);
                        self.active_servers.insert(server_name.clone());
                    }
                }
            }
        }

        if self.active_servers.is_empty() {
            return Err(anyhow!("No active servers"));
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

        let partial_report_timeout = Duration::from_secs(6);
        let partial_report_timer = tokio::time::sleep(partial_report_timeout);
        tokio::pin!(partial_report_timer);

        loop {
            let timeout_duration = Duration::from_millis(heartbeat_interval);
            let timeout = tokio::time::sleep(timeout_duration);
            tokio::pin!(timeout);

            tokio::select! {
                // 优先处理关闭信号
                _ = shutdown_rx.recv() => {
                    info!("Shutting down...");
                    break;
                }
                Some(msg) = self.message_rx.recv() => {
                    self.last_activity = Instant::now();
                    handle_message(&mut self, msg).await?;
                }
                Some((server_name, output)) = process_output_rx.recv() => {
                    self.last_activity = Instant::now();
                    handle_process_output(&mut self, &server_name, output).await?;
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
                _ = &mut partial_report_timer, if !self.tools_list_response_sent => {
                    // 标记所有未响应的服务器为已完成
                    for server_name in self.config.servers.keys() {
                        if !self.collected_servers.contains(server_name) {
                            warn!(
                                "Marking server {} as collected (timeout, no tools)",
                                server_name
                            );
                            self.collected_servers.insert(server_name.clone());
                        }
                    }

                    if self.pending_tools_list_request.is_some() && !self.tools.is_empty() {
                        info!("Partial tool collection timeout, reporting {} tools", self.tools.len());
                        reply_tools_list(&mut self).await?;
                    }
                }
            }

            if !self.tools_collected && self.collected_servers.len() == self.active_servers.len() {
                self.tools_collected = true;
                info!("All tools collected, total: {}", self.tools.len());

                if self.tools_list_response_sent {
                    notify_tools_changed(&mut self).await?;
                } else if self.pending_tools_list_request.is_some() {
                    reply_tools_list(&mut self).await?;
                }
            }
        }

        for sse_server in self.sse_servers.values_mut() {
            sse_server.stop().await;
        }

        self.shutdown().await?;
        Ok(())
    }

    pub async fn send_to_server(&mut self, server_name: &str, message: &str) -> anyhow::Result<()> {
        if let Some(process_stdin) = self.processes_stdin.get_mut(server_name) {
            let message = message.to_string() + "\n";
            process_stdin.write_all(message.as_bytes()).await?;
            return Ok(());
        }

        if let Some(sse_server) = self.sse_servers.get(server_name) {
            sse_server.send(message).await?;
            return Ok(());
        }

        Err(anyhow::anyhow!("Server not found: {}", server_name))
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
    use crate::config::{AppConfig, ConnectionConfig, MqttConfig, WebSocketConfig};
    use async_trait::async_trait;
    use serde_json::Value;
    use std::any::Any;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::time::Instant;

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

    fn create_test_config() -> BridgeConfig {
        BridgeConfig {
            app_config: AppConfig {
                websocket: WebSocketConfig {
                    enabled: false,
                    endpoint: "".to_string(),
                },
                mqtt: MqttConfig {
                    enabled: false,
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

    impl Bridge {
        pub async fn new_with_transports(
            config: BridgeConfig,
            transports: Vec<Box<dyn Transport>>,
        ) -> anyhow::Result<Self> {
            let connection_config = Arc::new(config.app_config.connection.clone());
            let (message_tx, message_rx) = mpsc::channel(100);

            if transports.is_empty() {
                return Err(anyhow!("No transports provided"));
            }

            let now = Instant::now();
            Ok(Self {
                config,
                transports,
                processes_stdin: HashMap::new(),
                sse_servers: HashMap::new(),
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
                tools_list_response_sent: false,
                active_servers: HashSet::new(),
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

    fn create_test_bridge() -> Bridge {
        let config = create_test_config();
        let (message_tx, message_rx) = mpsc::channel(100);

        Bridge {
            config,
            transports: vec![],
            sse_servers: HashMap::new(),
            processes_stdin: HashMap::new(),
            message_tx,
            message_rx,
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
    async fn test_bridge_without_transports() {
        let config = create_test_config();
        let bridge = Bridge::new_with_transports(config, vec![]).await;
        assert!(bridge.is_err());
        assert_eq!(bridge.err().unwrap().to_string(), "No transports provided");
    }

    #[tokio::test]
    async fn test_broadcast_message() {
        let mut bridge = create_test_bridge();

        let result = bridge
            .broadcast_message(r#"{"test": "message"}"#.to_string())
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_broadcast_empty_message() {
        let config = create_test_config();
        let mock_transport = Box::new(MockTransport);
        let mut bridge = Bridge::new_with_transports(config, vec![mock_transport])
            .await
            .unwrap();

        let result = bridge.broadcast_message("   ".to_string()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_broadcast_invalid_json() {
        let config = create_test_config();
        let mock_transport = Box::new(MockTransport);
        let mut bridge = Bridge::new_with_transports(config, vec![mock_transport])
            .await
            .unwrap();

        let result = bridge.broadcast_message("invalid json".to_string()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_shutdown() {
        let config = create_test_config();
        let mock_transport = Box::new(MockTransport);
        let mut bridge = Bridge::new_with_transports(config, vec![mock_transport])
            .await
            .unwrap();

        let result = bridge.shutdown().await;
        assert!(result.is_ok());
    }
}
