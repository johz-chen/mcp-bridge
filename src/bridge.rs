use crate::config::{BridgeConfig, ConnectionConfig};
use crate::process::ManagedProcess;
use crate::transports::{MqttTransport, Transport, WebSocketTransport};
use anyhow::{Context, anyhow};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, interval, sleep};
use tracing::{debug, error, info, warn};

pub struct Bridge {
    config: BridgeConfig,
    transports: Vec<Box<dyn Transport>>,
    processes_stdin: HashMap<String, ChildStdin>,
    #[allow(dead_code)] // 通过克隆传递使用
    message_tx: mpsc::Sender<Value>,
    message_rx: mpsc::Receiver<Value>,
    connection_config: Arc<ConnectionConfig>,
    is_connected: bool,
    reconnect_attempt: u32,
    initialized: bool,
    tools: HashMap<String, (String, Value)>, // key: prefixed_tool_name, value: (server_name, tool_value)
    tool_service_map: HashMap<String, (String, String)>, // 新增：快速映射工具名到服务
    last_activity: Instant,
    last_ping_sent: Instant,
    pending_tools_list_request: Option<Value>, // 存储待处理的tools/list请求
    tools_collected: bool,                     // 标记是否所有工具都已收集完成
    collected_servers: HashSet<String>,        // 跟踪已收集工具的服务
}

impl Bridge {
    /// 生成带前缀的工具名称
    /// 将服务器名称中的中划线替换为下划线，并添加 xzcli 前缀
    pub fn generate_prefixed_tool_name(&self, server_name: &str, tool_name: &str) -> String {
        let normalized_server_name = server_name.replace('-', "_");
        // 使用内嵌格式参数
        format!("{normalized_server_name}_xzcli_{tool_name}")
    }

    #[cfg(test)]
    pub fn new_test_instance(config: BridgeConfig) -> Self {
        use std::collections::{HashMap, HashSet};
        use std::sync::Arc;
        use tokio::sync::mpsc;
        use tokio::time::Instant;

        let connection_config = Arc::new(config.connection.clone());
        let (message_tx, message_rx) = mpsc::channel(100);

        Bridge {
            config,
            transports: Vec::new(),
            processes_stdin: HashMap::new(),
            message_tx,
            message_rx,
            connection_config,
            is_connected: false,
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

    /// 根据前缀工具名称获取原始工具名称和服务器名称
    fn get_original_tool_name(&self, prefixed_tool_name: &str) -> Option<(String, String)> {
        // 直接通过哈希映射查找
        self.tool_service_map.get(prefixed_tool_name).cloned()
    }

    pub async fn new(config: BridgeConfig) -> anyhow::Result<Self> {
        let connection_config = Arc::new(config.connection.clone());
        let (message_tx, message_rx) = mpsc::channel(100);

        // Initialize transports
        let mut transports: Vec<Box<dyn Transport>> = Vec::new();

        // Add WebSocket transport
        let ws = WebSocketTransport::new(config.endpoint.clone(), message_tx.clone())
            .await
            .with_context(|| "Failed to initialize WebSocket transport")?;
        transports.push(Box::new(ws));

        // Add MQTT transport if configured
        if let Some(mqtt_config) = config.mqtt.clone() {
            let mqtt = MqttTransport::new(mqtt_config, message_tx.clone())
                .await
                .context("Failed to initialize MQTT transport")?;
            transports.push(Box::new(mqtt));
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
            tool_service_map: HashMap::new(), // 初始化工具服务映射
            last_activity: now,
            last_ping_sent: now,
            pending_tools_list_request: None,
            tools_collected: false,
            collected_servers: HashSet::new(),
        })
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        info!("MCP Bridge started");

        // Create channel for process outputs
        let (process_output_tx, mut process_output_rx) = mpsc::channel(100);

        // Start all server processes
        let mut processes = HashMap::new();
        for (server_name, process_config) in self.config.servers.clone() {
            let mut process = ManagedProcess::new(&process_config)?;
            process
                .start(process_output_tx.clone(), server_name.clone())
                .await?;
            info!("Started server process: {}", server_name);

            // 正确取出 stdin 而不移动整个 process
            if let Some(stdin) = process.stdin.take() {
                self.processes_stdin.insert(server_name.clone(), stdin);
            }

            processes.insert(server_name, process);
        }

        // Start all transports
        for transport in &mut self.transports {
            transport.connect().await?;
        }
        self.is_connected = true;
        self.last_activity = Instant::now(); // 重置活动时间

        // 使用现有的 heartbeat_interval 配置
        let heartbeat_interval = self.connection_config.heartbeat_interval;
        let heartbeat_timeout = self.connection_config.heartbeat_timeout;
        let ping_interval = heartbeat_interval / 2; // 每半次心跳间隔发送一次ping

        let mut ping_interval_timer = interval(Duration::from_millis(ping_interval));

        // Main event loop
        loop {
            let timeout = tokio::time::sleep(Duration::from_millis(heartbeat_interval));
            tokio::pin!(timeout);

            tokio::select! {
                Some(msg) = self.message_rx.recv() => {
                    self.last_activity = Instant::now();
                    self.handle_message(msg).await?;
                }
                Some((server_name, output)) = process_output_rx.recv() => {
                    self.last_activity = Instant::now();
                    self.handle_process_output(&server_name, output).await?;
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("Shutting down...");
                    break;
                }
                _ = ping_interval_timer.tick() => {
                    // 发送心跳ping
                    self.send_ping().await?;
                }
                _ = &mut timeout => {
                    // 检查心跳超时
                    let elapsed = self.last_activity.elapsed();
                    if elapsed > Duration::from_millis(heartbeat_timeout) {
                        warn!("Connection timeout detected ({}ms > {}ms)",
                              elapsed.as_millis(), heartbeat_timeout);
                        self.reconnect().await?;
                    }
                }
            }
        }

        // Shutdown processes
        for (_, mut process) in processes {
            process.stop().await?;
        }

        self.shutdown().await?;
        Ok(())
    }

    async fn send_ping(&mut self) -> anyhow::Result<()> {
        // 检查是否需要发送ping
        let time_since_last_activity = self.last_activity.elapsed();
        let time_since_last_ping = self.last_ping_sent.elapsed();

        // 如果最近有活动或刚发送过ping，则跳过
        if time_since_last_activity
            < Duration::from_millis(self.connection_config.heartbeat_interval / 3)
            || time_since_last_ping
                < Duration::from_millis(self.connection_config.heartbeat_interval / 2)
        {
            return Ok(());
        }

        // 使用标准库生成时间戳
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

        self.broadcast_message(ping_message.to_string()).await?;
        self.last_ping_sent = Instant::now();
        debug!("Sent ping to keep connection alive");

        Ok(())
    }

    async fn handle_message(&mut self, msg: Value) -> anyhow::Result<()> {
        info!("<< Received message: {}", msg);

        // Handle ping requests
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            if method == "ping" {
                return self.handle_ping_request(&msg).await;
            }
        }

        // Handle initialize requests
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            if method == "initialize" {
                return self.initialize(msg).await;
            }
        }

        // Handle tools/list requests
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            if method == "tools/list" {
                return self.handle_tools_list_request(msg).await;
            }
        }

        // Handle tools/call requests
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            if method == "tools/call" {
                return self.handle_tool_call(msg).await;
            }
        }

        // Handle notifications/initialized
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            if method == "notifications/initialized" {
                info!("Received notifications/initialized");
                return Ok(());
            }
        }

        // Unknown message type
        warn!("Received unknown message type: {}", msg);
        Ok(())
    }

    async fn handle_ping_request(&mut self, msg: &Value) -> anyhow::Result<()> {
        // 使用原始消息中的ID回复
        if let Some(id) = msg.get("id").cloned() {
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            });

            self.broadcast_message(response.to_string()).await?;
            debug!("Sent pong response for ping");
        }

        // 更新最后活动时间
        self.last_activity = Instant::now();
        Ok(())
    }

    async fn initialize(&mut self, msg: Value) -> anyhow::Result<()> {
        info!("Handling initialize request");

        // 使用原始消息中的ID回复
        let id = msg["id"].clone();

        // 立即发送初始化响应
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

        self.broadcast_message(response.to_string()).await?;
        info!("Sent initialize response");

        // 标记为已初始化
        self.initialized = true;
        info!("Bridge initialized");

        // 重置工具收集状态
        self.tools.clear();
        self.tool_service_map.clear(); // 清空映射
        self.collected_servers.clear();
        self.tools_collected = false;

        // 为每个服务启动工具列表请求
        for (server_name, stdin) in &mut self.processes_stdin {
            // 发送 tools/list 请求
            let tools_request = json!({
                "jsonrpc": "2.0",
                "id": format!("tools-list-{}", server_name),
                "method": "tools/list"
            });

            let message = tools_request.to_string() + "\n";
            stdin.write_all(message.as_bytes()).await?;
            debug!("Sent tools/list request to server: {}", server_name);
        }

        Ok(())
    }

    async fn handle_tools_list_request(&mut self, msg: Value) -> anyhow::Result<()> {
        info!("Handling tools/list request");

        // 存储请求，等待所有工具收集完成
        self.pending_tools_list_request = Some(msg);

        // 如果所有工具已经收集完成，立即回复
        if self.tools_collected {
            self.reply_tools_list().await?;
        }

        Ok(())
    }

    async fn reply_tools_list(&mut self) -> anyhow::Result<()> {
        if let Some(request) = self.pending_tools_list_request.take() {
            let id = request["id"].clone();

            // 构建完整的工具列表
            let mut tools_list = Vec::new();
            for (prefixed_name, (_, tool_value)) in &self.tools {
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

            self.broadcast_message(response.to_string()).await?;
            info!("Sent tools/list response with {} tools", tools_list.len());
        }

        Ok(())
    }

    async fn handle_tool_call(&mut self, msg: Value) -> anyhow::Result<()> {
        // 使用原始消息中的ID回复
        let id = msg["id"].clone();
        let method = msg["method"].as_str().unwrap_or("");
        let params = msg["params"].as_object();

        let prefixed_tool_name = params.and_then(|p| p["name"].as_str()).unwrap_or("");
        let arguments = params.and_then(|p| p["arguments"].as_object());

        info!("Handling tool call: {} with id {}", prefixed_tool_name, id);

        // 解析前缀工具名称
        if let Some((server_name, original_tool_name)) =
            self.get_original_tool_name(prefixed_tool_name)
        {
            if let Some(stdin) = self.processes_stdin.get_mut(&server_name) {
                // 转发请求到服务（使用原始工具名称）
                let request = json!({
                    "jsonrpc": "2.0",
                    "id": id, // 保持原始ID
                    "method": method,
                    "params": {
                        "name": original_tool_name,
                        "arguments": arguments
                    }
                });

                let message = request.to_string() + "\n";
                stdin.write_all(message.as_bytes()).await?;
                info!(
                    "Forwarded tool call to server: {} (original: {})",
                    server_name, original_tool_name
                );
                return Ok(());
            }
        }

        // 工具未找到
        let error_response = json!({
            "jsonrpc": "2.0",
            "id": id, // 保持原始ID
            "error": {
                "code": -32601,
                "message": format!("Tool not found: {}", prefixed_tool_name)
            }
        });

        self.broadcast_message(error_response.to_string()).await?;
        warn!("Tool not found: {}", prefixed_tool_name);

        Ok(())
    }

    async fn handle_process_output(
        &mut self,
        server_name: &str,
        output: String,
    ) -> anyhow::Result<()> {
        debug!(">> Received output from {}: {}", server_name, output.trim());

        // 尝试解析为JSON
        let msg_value: Value = match serde_json::from_str(&output) {
            Ok(value) => value,
            Err(_) => Value::String(output.clone()),
        };

        // 处理工具列表响应
        let mut should_broadcast = true;
        if let Some(result) = msg_value.get("result") {
            if let Some(id) = msg_value.get("id") {
                if let Some(_method) = msg_value.get("method") {
                    // 这不是响应，是请求
                } else {
                    // 这是响应
                    if let Some(id_str) = id.as_str() {
                        if id_str.starts_with("tools-list-") {
                            should_broadcast = false; // 不广播原始工具列表响应

                            if let Some(tools) = result["tools"].as_array() {
                                // 记录此服务已响应
                                self.collected_servers.insert(server_name.to_string());

                                for tool in tools {
                                    if let Some(tool_obj) = tool.as_object() {
                                        if let Some(Value::String(original_tool_name)) =
                                            tool_obj.get("name")
                                        {
                                            // 生成带前缀的工具名称
                                            let prefixed_name = self.generate_prefixed_tool_name(
                                                server_name,
                                                original_tool_name,
                                            );

                                            // 存储工具信息
                                            self.tools.insert(
                                                prefixed_name.clone(),
                                                (server_name.to_string(), tool.clone()),
                                            );

                                            // 添加到快速映射表
                                            self.tool_service_map.insert(
                                                prefixed_name,
                                                (
                                                    server_name.to_string(),
                                                    original_tool_name.clone(),
                                                ),
                                            );
                                        }
                                    }
                                }

                                info!("Collected {} tools from {}", tools.len(), server_name);

                                // 检查是否所有服务都响应了
                                if self.collected_servers.len() == self.config.servers.len() {
                                    self.tools_collected = true;
                                    info!("All tools collected, total: {}", self.tools.len());

                                    // 回复待处理的请求
                                    self.reply_tools_list().await?;
                                }
                            }
                        }
                    }
                }
            }
        }

        // 如果不是工具列表响应，则广播原始消息
        if should_broadcast {
            // 广播到所有传输
            self.broadcast_message(msg_value.to_string()).await?;
        }

        // 更新最后活动时间
        self.last_activity = Instant::now();
        Ok(())
    }

    async fn broadcast_message(&mut self, msg: String) -> anyhow::Result<()> {
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
            self.handle_transport_disconnect(i).await;
        }

        // 更新最后活动时间
        self.last_activity = Instant::now();
        Ok(())
    }

    async fn handle_transport_disconnect(&mut self, index: usize) {
        if self.reconnect_attempt >= self.connection_config.max_reconnect_attempts {
            error!("Max reconnection attempts reached");
            return;
        }

        self.reconnect_attempt += 1;
        let delay = Duration::from_millis(self.connection_config.reconnect_interval);

        warn!(
            "Attempting to reconnect (attempt {}) in {:?}",
            self.reconnect_attempt, delay
        );

        tokio::time::sleep(delay).await;

        if let Err(e) = self.transports[index].connect().await {
            error!("Failed to reconnect transport: {}", e);
        } else {
            self.reconnect_attempt = 0;
        }
    }

    async fn reconnect(&mut self) -> anyhow::Result<()> {
        if self.reconnect_attempt >= self.connection_config.max_reconnect_attempts {
            error!("Max reconnection attempts reached");
            return Err(anyhow!("Max reconnection attempts reached"));
        }

        self.reconnect_attempt += 1;
        let delay = Duration::from_millis(self.connection_config.reconnect_interval);

        warn!(
            "Attempting to reconnect (attempt {}) in {:?}",
            self.reconnect_attempt, delay
        );

        sleep(delay).await;

        // Disconnect existing connections
        for transport in &mut self.transports {
            let _ = transport.disconnect().await;
        }

        // Reconnect
        for transport in &mut self.transports {
            if let Err(e) = transport.connect().await {
                error!("Failed to reconnect transport: {}", e);
            }
        }

        // Reset state
        self.is_connected = true;
        self.reconnect_attempt = 0;
        self.last_activity = Instant::now();
        self.last_ping_sent = Instant::now();

        info!("Successfully reconnected");
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        info!("Shutting down MCP Bridge");

        // Shutdown transports
        for transport in &mut self.transports {
            transport.disconnect().await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BridgeConfig, ConnectionConfig, ProcessConfig};
    use serde_json::json;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use tokio::runtime::Runtime;
    use tokio::sync::mpsc;
    use tokio::time::Instant;

    // 创建一个用于测试的 Bridge 实例，不初始化传输
    fn create_test_bridge(config: BridgeConfig) -> Bridge {
        let connection_config = Arc::new(config.connection.clone());
        let (message_tx, message_rx) = mpsc::channel(100);

        Bridge {
            config,
            transports: Vec::new(),
            processes_stdin: HashMap::new(),
            message_tx,
            message_rx,
            connection_config,
            is_connected: false,
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

    #[test]
    fn test_generate_prefixed_tool_name() {
        let config = BridgeConfig {
            endpoint: "wss://example.com".to_string(),
            servers: HashMap::new(),
            connection: ConnectionConfig::default(),
            mqtt: None,
        };

        let bridge = create_test_bridge(config);

        let name = bridge.generate_prefixed_tool_name("server-name", "tool");
        assert_eq!(name, "server_name_xzcli_tool");
    }

    #[test]
    fn test_get_original_tool_name() {
        let config = BridgeConfig {
            endpoint: "wss://example.com".to_string(),
            servers: [(
                "server1".to_string(),
                ProcessConfig {
                    command: "cmd".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                },
            )]
            .iter()
            .cloned()
            .collect(),
            connection: ConnectionConfig::default(),
            mqtt: None,
        };

        let mut bridge = create_test_bridge(config);

        // 添加测试工具
        bridge.tool_service_map.insert(
            "server1_xzcli_tool1".to_string(),
            ("server1".to_string(), "tool1".to_string()),
        );

        let result = bridge.get_original_tool_name("server1_xzcli_tool1");
        assert_eq!(result, Some(("server1".to_string(), "tool1".to_string())));

        let not_found = bridge.get_original_tool_name("unknown_tool");
        assert!(not_found.is_none());
    }

    #[test]
    fn test_handle_ping_request() {
        let (tx, _) = mpsc::channel(1);
        let config = BridgeConfig {
            endpoint: "wss://example.com".to_string(),
            servers: HashMap::new(),
            connection: ConnectionConfig::default(),
            mqtt: None,
        };

        let mut bridge = Bridge {
            config,
            transports: vec![],
            processes_stdin: HashMap::new(),
            message_tx: tx,
            message_rx: mpsc::channel(1).1,
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
        };

        let ping_msg = json!({
            "jsonrpc": "2.0",
            "id": 123,
            "method": "ping"
        });

        Runtime::new()
            .unwrap()
            .block_on(bridge.handle_ping_request(&ping_msg))
            .unwrap();
    }
}
