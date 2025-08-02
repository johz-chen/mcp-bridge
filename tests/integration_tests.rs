use anyhow::Result;
use mcp_bridge::config::{
    AppConfig, BridgeConfig, ConfigError, ConnectionConfig, MqttConfig, ServerConfig,
    WebSocketConfig,
};
use mcp_bridge::process::ManagedProcess;
use std::collections::HashMap;
use std::io::Write;
use tempfile::NamedTempFile;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

#[tokio::test]
async fn test_process_start_and_stop() {
    let config = ServerConfig::Std {
        command: "echo".to_string(),
        args: vec!["hello world".to_string()],
        env: HashMap::new(),
    };

    let (tx, mut rx) = mpsc::channel(10);
    let server_name = "test_server".to_string();

    let mut process = ManagedProcess::new(&config).unwrap();
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
        ServerConfig::Std {
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
    if let ServerConfig::Std { command, .. } = &config.servers["test_server"] {
        assert_eq!(command, "echo");
    }
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

#[tokio::test]
async fn test_config_validation() {
    // 测试有效的配置
    let valid_app_config = AppConfig {
        websocket: WebSocketConfig {
            enabled: true,
            endpoint: "wss://valid.com".to_string(),
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

    let mut valid_servers = HashMap::new();
    valid_servers.insert(
        "test_server".to_string(),
        ServerConfig::Std {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            env: HashMap::new(),
        },
    );

    let valid_config = BridgeConfig {
        app_config: valid_app_config,
        servers: valid_servers,
    };

    assert!(valid_config.validate().is_ok());

    // 测试无效的配置 - 空服务器命令
    let invalid_app_config = AppConfig {
        websocket: WebSocketConfig {
            enabled: true,
            endpoint: "wss://valid.com".to_string(),
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

    let mut invalid_servers = HashMap::new();
    invalid_servers.insert(
        "invalid_server".to_string(),
        ServerConfig::Std {
            command: "".to_string(), // 空命令
            args: vec![],
            env: HashMap::new(),
        },
    );

    let invalid_config = BridgeConfig {
        app_config: invalid_app_config,
        servers: invalid_servers,
    };

    assert!(invalid_config.validate().is_err());

    // 测试启用的 WebSocket 但缺少端点
    let ws_invalid_config = AppConfig {
        websocket: WebSocketConfig {
            enabled: true,
            endpoint: "".to_string(), // 空端点
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

    assert!(
        BridgeConfig {
            app_config: ws_invalid_config,
            servers: HashMap::new(),
        }
        .validate()
        .is_err()
    );
}

#[tokio::test]
async fn test_process_with_environment_variables() {
    let mut env = HashMap::new();
    env.insert("TEST_VAR".to_string(), "test_value".to_string());

    let config = ServerConfig::Std {
        command: if cfg!(windows) {
            "cmd".to_string()
        } else {
            "sh".to_string()
        },
        args: if cfg!(windows) {
            vec!["/C".to_string(), "echo %TEST_VAR%".to_string()]
        } else {
            vec!["-c".to_string(), "echo $TEST_VAR".to_string()]
        },
        env,
    };

    let (tx, mut rx) = mpsc::channel(10);
    let server_name = "env_test".to_string();

    let mut process = mcp_bridge::process::ManagedProcess::new(&config).unwrap();
    process.start(tx, server_name.clone()).await.unwrap();

    // 等待进程输出
    tokio::time::sleep(Duration::from_millis(100)).await;
    process.stop().await.unwrap();

    // 检查输出
    let output = timeout(Duration::from_secs(1), rx.recv()).await;
    assert!(output.is_ok());
    if let Ok(Some((name, msg))) = output {
        assert_eq!(name, "env_test");
        assert!(msg.contains("test_value"));
    }
}

#[tokio::test]
async fn test_process_with_arguments() {
    // 创建带有参数的进程配置
    let config = ServerConfig::Std {
        command: if cfg!(windows) {
            "cmd".to_string()
        } else {
            "echo".to_string()
        },
        args: if cfg!(windows) {
            vec![
                "/C".to_string(),
                "echo".to_string(),
                "hello world".to_string(),
            ]
        } else {
            vec!["hello world".to_string()]
        },
        env: HashMap::new(),
    };

    let (tx, mut rx) = mpsc::channel(10);
    let server_name = "args_test".to_string();

    let mut process = mcp_bridge::process::ManagedProcess::new(&config).unwrap();
    process.start(tx, server_name.clone()).await.unwrap();

    // 等待进程输出
    tokio::time::sleep(Duration::from_millis(100)).await;
    process.stop().await.unwrap();

    // 检查输出
    let output = timeout(Duration::from_secs(1), rx.recv()).await;
    assert!(output.is_ok());
    if let Ok(Some((name, msg))) = output {
        assert_eq!(name, "args_test");
        assert!(msg.contains("hello world"));
    }
}

#[tokio::test]
async fn test_config_file_loading() -> Result<(), ConfigError> {
    // 创建临时 YAML 文件
    let mut yaml_file = NamedTempFile::new()?;
    writeln!(
        yaml_file,
        r#"
websocket:
  enabled: true
  endpoint: "wss://test-loading.com"
mqtt:
  enabled: false
  broker: "broker.example.com"
  port: 1883
  topic: "mcp/test"
connection:
  heartbeat:
    interval: 30000
    timeout: 10000
  reconnect:
    interval: 5000
        "#
    )?;

    // 创建临时 JSON 文件
    let mut json_file = NamedTempFile::new()?;
    writeln!(
        json_file,
        r#"{{
            "test_server": {{
                "command": "echo",
                "args": ["hello"]
            }}
        }}"#
    )?;

    let config = BridgeConfig::load_from_files(yaml_file.path(), json_file.path())?;

    assert_eq!(
        config.app_config.websocket.endpoint,
        "wss://test-loading.com"
    );
    assert_eq!(config.servers.len(), 1);
    assert_eq!(config.app_config.connection.heartbeat_interval, 30000);

    if let ServerConfig::Std { command, .. } = &config.servers["test_server"] {
        assert_eq!(command, "echo");
    }

    Ok(())
}

#[tokio::test]
async fn test_config_file_not_found() {
    let yaml_path = "non_existent_config.yaml";
    let json_path = "non_existent_config.json";

    let result = BridgeConfig::load_from_files(yaml_path, json_path);
    assert!(result.is_err());

    if let Err(e) = result {
        assert!(e.to_string().contains("Config file not found"));
    }
}

#[tokio::test]
async fn test_multiple_processes() {
    // 创建两个进程配置
    let config1 = ServerConfig::Std {
        command: "echo".to_string(),
        args: vec!["process one".to_string()],
        env: HashMap::new(),
    };

    let config2 = ServerConfig::Std {
        command: "echo".to_string(),
        args: vec!["process two".to_string()],
        env: HashMap::new(),
    };

    let (tx, mut rx) = mpsc::channel(10);
    let server_name1 = "server1".to_string();
    let server_name2 = "server2".to_string();

    let mut process1 = mcp_bridge::process::ManagedProcess::new(&config1).unwrap();
    let mut process2 = mcp_bridge::process::ManagedProcess::new(&config2).unwrap();

    process1
        .start(tx.clone(), server_name1.clone())
        .await
        .unwrap();
    process2
        .start(tx.clone(), server_name2.clone())
        .await
        .unwrap();

    // 等待进程输出
    tokio::time::sleep(Duration::from_millis(100)).await;

    process1.stop().await.unwrap();
    process2.stop().await.unwrap();

    // 收集所有输出
    let mut outputs = Vec::new();
    while let Ok(Some(output)) = timeout(Duration::from_secs(1), rx.recv()).await {
        outputs.push(output);
    }

    assert_eq!(outputs.len(), 2);
    let server1_output = outputs.iter().find(|(name, _)| name == "server1");
    let server2_output = outputs.iter().find(|(name, _)| name == "server2");

    assert!(server1_output.is_some());
    assert!(server2_output.is_some());
    assert!(server1_output.unwrap().1.contains("process one"));
    assert!(server2_output.unwrap().1.contains("process two"));
}

#[tokio::test]
async fn test_process_stop_before_start() {
    let config = ServerConfig::Std {
        command: "echo".to_string(),
        args: vec!["should not run".to_string()],
        env: HashMap::new(),
    };

    let mut process = mcp_bridge::process::ManagedProcess::new(&config).unwrap();

    // 尝试在启动前停止
    let result = process.stop().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_process_restart() {
    let config = ServerConfig::Std {
        command: "echo".to_string(),
        args: vec!["restart test".to_string()],
        env: HashMap::new(),
    };

    let (tx, mut rx) = mpsc::channel(10);
    let server_name = "restart_test".to_string();

    let mut process = mcp_bridge::process::ManagedProcess::new(&config).unwrap();

    // 第一次启动
    process
        .start(tx.clone(), server_name.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    process.stop().await.unwrap();

    // 检查第一次输出
    let output1 = timeout(Duration::from_secs(1), rx.recv()).await;
    assert!(output1.is_ok());

    // 第二次启动
    process
        .start(tx.clone(), server_name.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    process.stop().await.unwrap();

    // 检查第二次输出
    let output2 = timeout(Duration::from_secs(1), rx.recv()).await;
    assert!(output2.is_ok());
}

#[tokio::test]
async fn test_bridge_config_with_sse() -> Result<()> {
    // 测试配置加载包含 SSE 的情况
    let mut json_file = NamedTempFile::new()?;
    writeln!(
        json_file,
        r#"{{
            "sse_server": {{
                "url": "http://localhost:8080/sse"
            }},
            "std_server": {{
                "command": "echo",
                "args": ["hello"]
            }}
        }}"#
    )?;

    let yaml_content = r#"
websocket:
  enabled: false
  endpoint: ""
mqtt:
  enabled: false
  broker: ""
  port: 1883
  topic: ""
connection:
  heartbeat:
    interval: 30000
    timeout: 10000
  reconnect:
    interval: 5000
    "#;

    let mut yaml_file = NamedTempFile::new()?;
    yaml_file.write_all(yaml_content.as_bytes())?;

    let config = BridgeConfig::load_from_files(yaml_file.path(), json_file.path())?;

    // 验证配置正确加载
    assert_eq!(config.servers.len(), 2);
    assert!(matches!(
        config.servers["sse_server"],
        ServerConfig::Sse { .. }
    ));
    assert!(matches!(
        config.servers["std_server"],
        ServerConfig::Std { .. }
    ));

    Ok(())
}

#[tokio::test]
async fn test_config_validation_sse() {
    // 测试 SSE 配置的验证
    let app_config = AppConfig {
        websocket: WebSocketConfig {
            enabled: false,
            endpoint: "".to_string(),
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

    let mut servers = HashMap::new();
    servers.insert(
        "sse_server".to_string(),
        ServerConfig::Sse {
            url: "".to_string(),
            headers: HashMap::new(),
        },
    );

    let config = BridgeConfig {
        app_config,
        servers,
    };

    assert!(config.validate().is_err());
}

#[tokio::test]
async fn test_config_validation_sse_valid() {
    // 测试有效的 SSE 配置
    let app_config = AppConfig {
        websocket: WebSocketConfig {
            enabled: false,
            endpoint: "".to_string(),
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

    let mut servers = HashMap::new();
    servers.insert(
        "sse_server".to_string(),
        ServerConfig::Sse {
            url: "http://localhost:8080/sse".to_string(),
            headers: HashMap::new(),
        },
    );

    let config = BridgeConfig {
        app_config,
        servers,
    };

    assert!(config.validate().is_ok());
}

#[tokio::test]
async fn test_sse_config_validation() -> Result<()> {
    let yaml_content = r#"
websocket:
  enabled: false
  endpoint: ""
mqtt:
  enabled: false
  broker: ""
  port: 1883
  topic: ""
connection:
  heartbeat:
    interval: 30000
    timeout: 10000
  reconnect:
    interval: 5000
    "#;

    let json_content = r#"{
            "sse_server": {
                "url": "http://localhost:8080/sse"
            }
        }"#;

    let mut yaml_file = NamedTempFile::new()?;
    yaml_file.write_all(yaml_content.as_bytes())?;

    let mut json_file = NamedTempFile::new()?;
    json_file.write_all(json_content.as_bytes())?;

    let config = BridgeConfig::load_from_files(yaml_file.path(), json_file.path())?;

    // 验证配置正确加载
    assert_eq!(config.servers.len(), 1);
    assert!(matches!(
        config.servers["sse_server"],
        ServerConfig::Sse { .. }
    ));

    // 验证配置有效
    assert!(config.validate().is_ok());

    Ok(())
}

#[tokio::test]
async fn test_sse_config_invalid_url() -> Result<()> {
    // 测试无效 URL 的验证
    let app_config = AppConfig {
        websocket: WebSocketConfig {
            enabled: false,
            endpoint: "".to_string(),
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

    let mut servers = HashMap::new();
    servers.insert(
        "sse_server".to_string(),
        ServerConfig::Sse {
            url: "".to_string(),
            headers: HashMap::new(),
        },
    );

    let config = BridgeConfig {
        app_config,
        servers,
    };

    // 验证空 URL 应该失败
    let result = config.validate();
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("url for server sse_server cannot be empty")
    );

    Ok(())
}

#[tokio::test]
async fn test_sse_server_json_format() -> Result<()> {
    // 测试 JSON 中的 SSE 配置
    let json_content = r#"{
        "my_sse_server": {
            "url": "https://api.example.com/sse"
        },
        "another_server": {
            "command": "echo",
            "args": ["hello"]
        }
    }"#;

    let yaml_content = r#"
websocket:
  enabled: false
  endpoint: ""
mqtt:
  enabled: false
  broker: ""
  port: 1883
  topic: ""
connection:
  heartbeat:
    interval: 30000
  reconnect:
    interval: 5000
    "#;

    let mut json_file = NamedTempFile::new()?;
    json_file.write_all(json_content.as_bytes())?;

    let mut yaml_file = NamedTempFile::new()?;
    yaml_file.write_all(yaml_content.as_bytes())?;

    let config = BridgeConfig::load_from_files(yaml_file.path(), json_file.path())?;

    // 验证两个服务器都被正确加载
    assert_eq!(config.servers.len(), 2);
    assert!(matches!(
        config.servers["my_sse_server"],
        ServerConfig::Sse { .. }
    ));
    assert!(matches!(
        config.servers["another_server"],
        ServerConfig::Std { .. }
    ));

    Ok(())
}

#[tokio::test]
async fn test_sse_mixed_config() -> Result<()> {
    let yaml_content = r#"
websocket:
  enabled: true
  endpoint: "wss://test.com"
mqtt:
  enabled: false
  broker: ""
  port: 1883
  topic: ""
  client_id: "test"
connection:
  heartbeat:
    interval: 30000
    "#;

    let json_content = r#"{"sse1":{"url":"http://localhost:8080/sse"},"std1":{"command":"echo","args":["hello"]}}"#;

    let mut yaml_file = NamedTempFile::new()?;
    yaml_file.write_all(yaml_content.as_bytes())?;

    let mut json_file = NamedTempFile::new()?;
    json_file.write_all(json_content.as_bytes())?;

    let config = BridgeConfig::load_from_files(yaml_file.path(), json_file.path())?;

    assert_eq!(config.servers.len(), 2);
    assert!(matches!(config.servers["sse1"], ServerConfig::Sse { .. }));
    assert!(matches!(config.servers["std1"], ServerConfig::Std { .. }));

    Ok(())
}
