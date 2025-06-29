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
}

// 连接配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectionConfig {
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: u64,  // 毫秒
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout: u64,   // 毫秒
    #[serde(default = "default_reconnect_interval")]
    pub reconnect_interval: u64,  // 毫秒
    #[serde(default = "default_max_reconnect_attempts")]
    pub max_reconnect_attempts: u32,
}

fn default_heartbeat_interval() -> u64 { 30000 }
fn default_heartbeat_timeout() -> u64 { 10000 }
fn default_reconnect_interval() -> u64 { 5000 }
fn default_max_reconnect_attempts() -> u32 { 10 }

// 本地进程配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessConfig {
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

// MQTT 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttConfig {
    pub broker: String,
    pub port: u16,
    #[serde(default = "default_mqtt_client_id")]
    pub client_id: String,
    pub topic: String,
}

fn default_mqtt_client_id() -> String {
    format!("mcp-bridge-{}", std::process::id())
}

// 主配置结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    #[serde(rename = "mcpEndpoint")]
    pub endpoint: String,
    #[serde(rename = "mcpServers")]
    pub servers: HashMap<String, ProcessConfig>,
    #[serde(rename = "connection", default)]
    pub connection: ConnectionConfig,
    #[serde(default)]
    pub mqtt: Option<MqttConfig>, // 可选的 MQTT 配置
}

impl BridgeConfig {
    /// 从文件加载配置
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(ConfigError::NotFound(path.display().to_string()));
        }

        let content = fs::read_to_string(path)?;
        let config: Self = serde_json::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    /// 验证配置有效性
    fn validate(&self) -> Result<(), ConfigError> {
        if self.endpoint.is_empty() {
            return Err(ConfigError::InvalidFormat("mcpEndpoint cannot be empty".into()));
        }

        if self.servers.is_empty() {
            return Err(ConfigError::InvalidFormat("mcpServers cannot be empty".into()));
        }

        for (name, server) in &self.servers {
            if server.command.is_empty() {
                return Err(ConfigError::InvalidFormat(
                    format!("process.command for server {} cannot be empty", name).into()
                ));
            }
        }
        
        if let Some(mqtt) = &self.mqtt {
            if mqtt.broker.is_empty() {
                return Err(ConfigError::InvalidFormat("mqtt.broker cannot be empty".into()));
            }
            if mqtt.topic.is_empty() {
                return Err(ConfigError::InvalidFormat("mqtt.topic cannot be empty".into()));
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
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
            "mcpEndpoint": "wss://example.com/mcp",
            "mcpServers": {{
                "test_server": {{
                    "command": "echo",
                    "args": ["hello"]
                }}
            }},
            "connection": {{
                "heartbeatInterval": 30000,
                "heartbeatTimeout": 10000,
                "reconnectInterval": 5000,
                "max_reconnect_attempts": 10
            }}
        }}"#
        )
        .unwrap();

        let config = BridgeConfig::load_from_file(file.path()).unwrap();
        assert_eq!(config.endpoint, "wss://example.com/mcp");
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.servers["test_server"].command, "echo");
        assert_eq!(config.connection.heartbeat_interval, 30000);
        assert_eq!(config.connection.max_reconnect_attempts, 10);
    }

    #[test]
    fn test_load_invalid_config() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "invalid json").unwrap();

        let result = BridgeConfig::load_from_file(file.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_config() {
        let config = BridgeConfig {
            endpoint: "".to_string(),
            servers: HashMap::new(),
            connection: ConnectionConfig::default(),
            mqtt: None,
        };

        let result = config.validate();
        assert!(result.is_err());
    }
}
