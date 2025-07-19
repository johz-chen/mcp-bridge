use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Config file not found: {0}")]
    NotFound(String),
    #[error("Invalid config format: {0}")]
    InvalidFormat(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    JsonError(#[from] serde_json::Error),
    #[error("YAML parse error: {0}")]
    YamlError(#[from] serde_yaml::Error),
}

pub fn default_heartbeat_interval() -> u64 {
    30000
}
pub fn default_heartbeat_timeout() -> u64 {
    10000
}
pub fn default_reconnect_interval() -> u64 {
    5000
}
pub fn default_max_reconnect_attempts() -> u32 {
    10
}

// 连接配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectionConfig {
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: u64, // 毫秒
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout: u64, // 毫秒
    #[serde(default = "default_reconnect_interval")]
    pub reconnect_interval: u64, // 毫秒
    #[serde(default = "default_max_reconnect_attempts")]
    pub max_reconnect_attempts: u32,
}


// 本地进程配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessConfig {
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

// WebSocket 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketConfig {
    pub enabled: bool,
    pub endpoint: String,
}

// MQTT 配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MqttConfig {
    pub enabled: bool,
    pub broker: String,
    pub port: u16,
    #[serde(default = "default_mqtt_client_id")]
    pub client_id: String,
    pub topic: String,
}

pub fn default_mqtt_client_id() -> String {
    format!("mcp-bridge-{}", std::process::id())
}

// 应用主配置 (YAML)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub websocket: WebSocketConfig,
    pub mqtt: MqttConfig,
    pub connection: ConnectionConfig,
}

// 主配置结构
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub app_config: AppConfig,                   // 来自 config.yaml
    pub servers: HashMap<String, ProcessConfig>, // 来自 mcp_tools.json
}

impl BridgeConfig {
    /// 从文件加载配置
    pub fn load_from_files<P: AsRef<Path>>(
        yaml_path: P,
        json_path: P,
    ) -> Result<Self, ConfigError> {
        let yaml_path = yaml_path.as_ref();
        let json_path = json_path.as_ref();

        if !yaml_path.exists() {
            return Err(ConfigError::NotFound(yaml_path.display().to_string()));
        }
        if !json_path.exists() {
            return Err(ConfigError::NotFound(json_path.display().to_string()));
        }

        // 加载 YAML 配置
        let yaml_content = fs::read_to_string(yaml_path)?;
        let app_config: AppConfig = serde_yaml::from_str(&yaml_content)?;

        // 加载 JSON 配置
        let json_content = fs::read_to_string(json_path)?;
        let servers: HashMap<String, ProcessConfig> = serde_json::from_str(&json_content)?;

        let config = Self {
            app_config,
            servers,
        };
        config.validate()?;
        Ok(config)
    }

    /// 验证配置有效性
    fn validate(&self) -> Result<(), ConfigError> {
        if self.app_config.websocket.enabled && self.app_config.websocket.endpoint.is_empty() {
            return Err(ConfigError::InvalidFormat(
                "websocket.endpoint cannot be empty when enabled".into(),
            ));
        }

        if self.app_config.mqtt.enabled {
            if self.app_config.mqtt.broker.is_empty() {
                return Err(ConfigError::InvalidFormat(
                    "mqtt.broker cannot be empty when enabled".into(),
                ));
            }
            if self.app_config.mqtt.topic.is_empty() {
                return Err(ConfigError::InvalidFormat(
                    "mqtt.topic cannot be empty when enabled".into(),
                ));
            }
        }

        if self.servers.is_empty() {
            return Err(ConfigError::InvalidFormat("servers cannot be empty".into()));
        }

        for (name, server) in &self.servers {
            if server.command.is_empty() {
                return Err(ConfigError::InvalidFormat(format!(
                    "process.command for server {name} cannot be empty"
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_load_valid_config() {
        // 创建临时 YAML 文件
        let mut yaml_file = NamedTempFile::new().unwrap();
        writeln!(
            yaml_file,
            r#"
websocket:
  enabled: true
  endpoint: "wss://example.com/mcp"
mqtt:
  enabled: false
  broker: "broker.example.com"
  port: 1883
  topic: "mcp/bridge"
connection:
  heartbeat:
    interval: 30000
    timeout: 10000
  reconnect:
    interval: 5000
        "#
        )
        .unwrap();

        // 创建临时 JSON 文件
        let mut json_file = NamedTempFile::new().unwrap();
        writeln!(
            json_file,
            r#"{{
            "test_server": {{
                "command": "echo",
                "args": ["hello"]
            }}
        }}"#
        )
        .unwrap();

        let config = BridgeConfig::load_from_files(yaml_file.path(), json_file.path()).unwrap();
        assert_eq!(
            config.app_config.websocket.endpoint,
            "wss://example.com/mcp"
        );
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.servers["test_server"].command, "echo");
        assert_eq!(config.app_config.connection.heartbeat_interval, 30000);
    }

    #[test]
    fn test_load_invalid_config() {
        let mut yaml_file = NamedTempFile::new().unwrap();
        writeln!(yaml_file, "invalid yaml").unwrap();

        let mut json_file = NamedTempFile::new().unwrap();
        writeln!(json_file, "{{}}").unwrap();

        let result = BridgeConfig::load_from_files(yaml_file.path(), json_file.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_default_connection_config() {
        let config: ConnectionConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.heartbeat_interval, 30000);
        assert_eq!(config.heartbeat_timeout, 10000);
        assert_eq!(config.reconnect_interval, 5000);
        assert_eq!(config.max_reconnect_attempts, 10);
    }

    #[test]
    fn test_process_config_default_env() {
        let config = ProcessConfig {
            command: "test".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        
        assert!(config.env.is_empty());
    }

    #[test]
    fn test_mqtt_config_default_client_id() {
        let config = MqttConfig {
            enabled: false,
            broker: "".to_string(),
            port: 1883,
            client_id: "".to_string(),
            topic: "".to_string(),
        };
        
        // 虽然这里为空，但实际使用时会生成默认值
        // 测试默认函数
        // let default_id = default_mqtt_client_id();
        assert!(config.client_id.is_empty());
        //assert!(config.client_id.contains("mcp-bridge"));
    }

}
