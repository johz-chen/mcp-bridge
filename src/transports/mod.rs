pub mod mqtt;
pub mod websocket;

use async_trait::async_trait;
use serde_json::Value;
use std::any::Any;
use std::fmt::Debug;

#[async_trait]
pub trait Transport: Send + Debug {
    async fn connect(&mut self) -> anyhow::Result<()>;
    async fn disconnect(&mut self) -> anyhow::Result<()>;
    async fn send(&mut self, msg: Value) -> anyhow::Result<()>;
    fn is_connected(&self) -> bool;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

pub use mqtt::MqttTransport;
pub use websocket::WebSocketTransport;
