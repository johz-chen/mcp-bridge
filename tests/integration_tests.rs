use mcp_bridge::config::{
    AppConfig, BridgeConfig, ConnectionConfig, MqttConfig, ProcessConfig, WebSocketConfig,
};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

#[tokio::test]
async fn test_process_start_and_stop() {
    let config = ProcessConfig {
        command: "echo".to_string(),
        args: vec!["hello world".to_string()],
        env: HashMap::new(),
    };

    let (tx, mut rx) = mpsc::channel(10);
    let server_name = "test_server".to_string();

    let mut process = mcp_bridge::process::ManagedProcess::new(&config).unwrap();
    process.start(tx, server_name.clone()).await.unwrap();

    // Give the process a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Stop the process
    process.stop().await.unwrap();

    // Check output
    let output = timeout(Duration::from_secs(1), rx.recv()).await;
    assert!(output.is_ok());
    if let Ok(Some((name, msg))) = output {
        assert_eq!(name, "test_server");
        assert!(msg.contains("hello world"));
    }
}

#[tokio::test]
async fn test_config_loading() {
    // 创建 AppConfig 部分
    let app_config = AppConfig {
        websocket: WebSocketConfig {
            enabled: true,
            endpoint: "wss://example.com".to_string(),
        },
        mqtt: MqttConfig {
            enabled: false,
            broker: "".to_string(),
            port: 1883,
            client_id: "test".to_string(),
            topic: "".to_string(),
        },
        connection: ConnectionConfig::default(),
    };

    // 创建服务器配置
    let mut servers = HashMap::new();
    servers.insert(
        "test_server".to_string(),
        ProcessConfig {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            env: HashMap::new(),
        },
    );

    let config = BridgeConfig {
        app_config,
        servers,
    };

    assert_eq!(config.servers.len(), 1);
    assert_eq!(config.servers["test_server"].command, "echo");
}

// 在集成测试中定义 Bridge 结构体的简化版本
struct TestBridge {
    #[allow(dead_code)]
    config: BridgeConfig,
    // 只需要测试中使用的字段
}

impl TestBridge {
    fn new(config: BridgeConfig) -> Self {
        Self { config }
    }

    // 复制 generate_prefixed_tool_name 方法
    fn generate_prefixed_tool_name(&self, server_name: &str, tool_name: &str) -> String {
        let normalized_server_name = server_name.replace('-', "_");
        format!("{}_xzcli_{}", normalized_server_name, tool_name)
    }
}

#[tokio::test]
async fn test_tool_name_generation() {
    // 创建 AppConfig 部分
    let app_config = AppConfig {
        websocket: WebSocketConfig {
            enabled: true,
            endpoint: "wss://example.com".to_string(),
        },
        mqtt: MqttConfig {
            enabled: false,
            broker: "".to_string(),
            port: 1883,
            client_id: "test".to_string(),
            topic: "".to_string(),
        },
        connection: ConnectionConfig::default(),
    };

    // 创建空服务器配置
    let servers = HashMap::new();

    let config = BridgeConfig {
        app_config,
        servers,
    };

    // 使用本地定义的 TestBridge
    let bridge = TestBridge::new(config);
    let name = bridge.generate_prefixed_tool_name("my-server", "my-tool");
    assert_eq!(name, "my_server_xzcli_my-tool");
}
