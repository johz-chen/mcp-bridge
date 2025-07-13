use super::Transport;
use crate::config::MqttConfig;
use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use serde_json::Value;
use std::any::Any;
use std::fmt;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::debug;

#[derive(Debug)]
pub struct MqttTransport {
    config: MqttConfig,
    client: Option<AsyncClient>,
    #[allow(dead_code)]
    tx: mpsc::Sender<Value>,
    is_connected: bool,
}

impl MqttTransport {
    pub async fn new(config: MqttConfig, tx: mpsc::Sender<Value>) -> anyhow::Result<Self> {
        let mut mqttoptions = MqttOptions::new(&config.client_id, &config.broker, config.port);
        mqttoptions.set_keep_alive(Duration::from_secs(30));

        let (client, mut event_loop) = AsyncClient::new(mqttoptions, 100);

        // 启动消息接收器
        let tx_clone = tx.clone();
        tokio::spawn(async move {
            while let Ok(event) = event_loop.poll().await {
                if let Event::Incoming(Incoming::Publish(publish)) = event {
                    debug!(
                        "Received MQTT message on topic {}: {} bytes",
                        publish.topic,
                        publish.payload.len()
                    );
                    if let Ok(msg) = serde_json::from_slice(&publish.payload) {
                        if tx_clone.send(msg).await.is_err() {
                            break; // 通道已关闭
                        }
                    } else {
                        debug!("Failed to parse MQTT message as JSON");
                    }
                }
            }
        });

        Ok(Self {
            config,
            client: Some(client),
            tx,
            is_connected: false,
        })
    }
}

#[async_trait]
impl Transport for MqttTransport {
    async fn connect(&mut self) -> anyhow::Result<()> {
        if let Some(client) = &self.client {
            debug!("Subscribing to MQTT topic: {}", self.config.topic);
            client
                .subscribe(&self.config.topic, QoS::AtLeastOnce)
                .await?;
            self.is_connected = true;
        }
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        if let Some(client) = &self.client {
            debug!("Disconnecting from MQTT broker");
            client.disconnect().await?;
        }
        self.client = None;
        self.is_connected = false;
        Ok(())
    }

    async fn send(&mut self, msg: Value) -> anyhow::Result<()> {
        if let Some(client) = &self.client {
            let msg_str = msg.to_string();
            debug!(
                "Sending MQTT message to topic {}: {}",
                self.config.topic, msg_str
            );
            client
                .publish(
                    &self.config.topic,
                    QoS::AtLeastOnce,
                    false,
                    msg_str.as_bytes(),
                )
                .await?;
        }
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.is_connected
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl fmt::Display for MqttTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MqttTransport({}:{})",
            self.config.broker, self.config.port
        )
    }
}
