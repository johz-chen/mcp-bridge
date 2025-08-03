use super::Transport;
use anyhow::anyhow;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::any::Any;
use std::fmt;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::protocol::{CloseFrame, Message as WsMessage},
};
use tracing::{debug, error, warn};

// 定义类型别名以简化复杂类型
type WebSocketSink =
    futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, WsMessage>;

#[derive(Debug)]
pub struct WebSocketTransport {
    endpoint: String,
    writer: Arc<Mutex<Option<WebSocketSink>>>,
    tx: mpsc::Sender<Value>,
    is_connected: bool,
}

impl WebSocketTransport {
    pub async fn new(endpoint: String, tx: mpsc::Sender<Value>) -> anyhow::Result<Self> {
        debug!("Connecting to WebSocket endpoint: {}", endpoint);

        // 添加连接超时
        let connect_result = timeout(Duration::from_secs(10), connect_async(&endpoint)).await;
        let (ws_stream, _) = match connect_result {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => return Err(anyhow!("WebSocket connection failed: {}", e)),
            Err(_) => return Err(anyhow!("WebSocket connection timed out")),
        };

        debug!("WebSocket connection established");

        let (write, read) = ws_stream.split();
        let writer = Arc::new(Mutex::new(Some(write)));

        // 启动消息接收任务
        let tx_clone = tx.clone();
        tokio::spawn(Self::receive_messages(read, tx_clone));

        Ok(Self {
            endpoint,
            writer,
            tx,
            is_connected: true,
        })
    }

    async fn receive_messages(
        mut read: futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
        tx: mpsc::Sender<Value>,
    ) {
        while let Some(msg) = read.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    debug!("Received WebSocket message: {}", text);
                    if let Ok(value) = serde_json::from_str(&text) {
                        if tx.send(value).await.is_err() {
                            error!("Failed to send message to bridge channel");
                            break;
                        }
                    } else {
                        warn!("Failed to parse WebSocket message as JSON: {}", text);
                    }
                }
                Ok(WsMessage::Binary(data)) => {
                    debug!("Received binary message: {} bytes", data.len());
                }
                Ok(WsMessage::Ping(data)) => {
                    debug!("Received WebSocket ping: {} bytes", data.len());
                }
                Ok(WsMessage::Pong(data)) => {
                    debug!("Received WebSocket pong: {} bytes", data.len());
                }
                Ok(WsMessage::Close(reason)) => {
                    if let Some(reason) = &reason {
                        debug!(
                            "WebSocket connection closed: {} - {}",
                            reason.code, reason.reason
                        );
                    } else {
                        debug!("WebSocket connection closed without reason");
                    }
                    break;
                }
                Err(e) => {
                    error!("WebSocket read error: {}", e);
                    break;
                }
                _ => {}
            }
        }
        debug!("WebSocket receive task exiting");
    }

    async fn connect_with_timeout(&mut self) -> anyhow::Result<()> {
        if self.is_connected {
            return Ok(());
        }

        debug!("Reconnecting to WebSocket endpoint: {}", self.endpoint);

        let connect_result = timeout(Duration::from_secs(10), connect_async(&self.endpoint)).await;
        match connect_result {
            Ok(Ok((ws_stream, _))) => {
                let (write, read) = ws_stream.split();
                *self.writer.lock().await = Some(write);
                self.is_connected = true;

                let tx_clone = self.tx.clone();
                tokio::spawn(Self::receive_messages(read, tx_clone));

                debug!("WebSocket reconnected");
                Ok(())
            }
            Ok(Err(e)) => {
                error!("WebSocket reconnection failed: {}", e);
                self.is_connected = false;
                Err(anyhow::anyhow!("WebSocket reconnection failed: {}", e))
            }
            Err(_) => {
                error!("WebSocket reconnection timed out");
                self.is_connected = false;
                Err(anyhow::anyhow!("WebSocket reconnection timed out"))
            }
        }
    }
}

#[async_trait]
impl Transport for WebSocketTransport {
    async fn connect(&mut self) -> anyhow::Result<()> {
        self.connect_with_timeout().await
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        if let Some(mut writer) = self.writer.lock().await.take() {
            let close_frame = CloseFrame {
                code: 1000.into(),
                reason: "Normal closure".into(),
            };
            debug!("Closing WebSocket connection");

            writer.send(WsMessage::Close(Some(close_frame))).await?;
        }
        self.is_connected = false;
        Ok(())
    }

    async fn send(&mut self, msg: Value) -> anyhow::Result<()> {
        if !self.is_connected {
            return Err(anyhow::anyhow!("WebSocket not connected"));
        }

        let mut writer = self.writer.lock().await;
        if let Some(writer) = writer.as_mut() {
            let msg_str = msg.to_string();
            debug!("Sending WebSocket message: {}", msg_str);
            if let Err(e) = writer.send(WsMessage::Text(msg_str)).await {
                error!("Failed to send WebSocket message: {}", e);
                self.is_connected = false;
                return Err(e.into());
            }
        } else {
            self.is_connected = false;
            return Err(anyhow::anyhow!("WebSocket writer is None"));
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

impl fmt::Display for WebSocketTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WebSocketTransport({})", self.endpoint)
    }
}
