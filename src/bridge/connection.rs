use super::core::Bridge;
use serde_json::json;
use tokio::time::{sleep, Duration, Instant};
use anyhow::anyhow;
use tracing::{debug, error, info, warn};

pub async fn send_ping(bridge: &mut Bridge) -> anyhow::Result<()> {
    let time_since_last_activity = bridge.last_activity.elapsed();
    let time_since_last_ping = bridge.last_ping_sent.elapsed();
    
    if time_since_last_activity < Duration::from_millis(bridge.connection_config.heartbeat_interval / 3) ||
       time_since_last_ping < Duration::from_millis(bridge.connection_config.heartbeat_interval / 2) {
        return Ok(());
    }
    
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
    
    bridge.broadcast_message(ping_message.to_string()).await?;
    bridge.last_ping_sent = Instant::now();
    debug!("Sent ping to keep connection alive");
    
    Ok(())
}

pub async fn reconnect(bridge: &mut Bridge) -> anyhow::Result<()> {
    if bridge.reconnect_attempt >= bridge.connection_config.max_reconnect_attempts {
        error!("Max reconnection attempts reached");
        return Err(anyhow!("Max reconnection attempts reached"));
    }
    
    bridge.reconnect_attempt += 1;
    let delay = Duration::from_millis(bridge.connection_config.reconnect_interval);
    
    warn!("Attempting to reconnect (attempt {}) in {:?}", 
        bridge.reconnect_attempt, delay);
    
    sleep(delay).await;
    
    for transport in &mut bridge.transports {
        let _ = transport.disconnect().await;
    }
    
    for transport in &mut bridge.transports {
        if let Err(e) = transport.connect().await {
            error!("Failed to reconnect transport: {}", e);
        }
    }
    
    bridge.is_connected = true;
    bridge.reconnect_attempt = 0;
    bridge.last_activity = Instant::now();
    bridge.last_ping_sent = Instant::now();
    
    info!("Successfully reconnected");
    Ok(())
}

pub async fn handle_transport_disconnect(bridge: &mut Bridge, index: usize) {
    if bridge.reconnect_attempt >= bridge.connection_config.max_reconnect_attempts {
        error!("Max reconnection attempts reached");
        return;
    }
    
    bridge.reconnect_attempt += 1;
    let delay = Duration::from_millis(bridge.connection_config.reconnect_interval);
    
    warn!("Attempting to reconnect (attempt {}) in {:?}", 
        bridge.reconnect_attempt, delay);
    
    tokio::time::sleep(delay).await;
    
    if let Err(e) = bridge.transports[index].connect().await {
        error!("Failed to reconnect transport: {}", e);
    } else {
        bridge.reconnect_attempt = 0;
    }
}
