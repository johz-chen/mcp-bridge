use super::core::Bridge;
use anyhow::Result;
use serde_json::{json, Value};
use tracing::{info, debug, warn};
use tokio::io::AsyncWriteExt;
use tokio::time::Instant;

pub async fn handle_message(bridge: &mut Bridge, msg: Value) -> Result<()> {
    info!("<< Received message: {}", msg);
    
    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
        match method {
            "ping" => return handle_ping_request(bridge, &msg).await,
            "initialize" => return initialize(bridge, msg).await,
            "tools/list" => return handle_tools_list_request(bridge, msg).await,
            "tools/call" => return handle_tool_call(bridge, msg).await,
            "notifications/initialized" => {
                info!("Received notifications/initialized");
                return Ok(());
            }
            _ => {}
        }
    }
    
    warn!("Received unknown message type: {}", msg);
    Ok(())
}

async fn handle_ping_request(bridge: &mut Bridge, msg: &Value) -> Result<()> {
    if let Some(id) = msg.get("id").cloned() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        });
        
        bridge.broadcast_message(response.to_string()).await?;
        debug!("Sent pong response for ping");
    }
    
    bridge.last_activity = Instant::now();
    Ok(())
}

async fn initialize(bridge: &mut Bridge, msg: Value) -> Result<()> {
    info!("Handling initialize request");
    
    let id = msg["id"].clone();
    
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
    
    bridge.broadcast_message(response.to_string()).await?;
    info!("Sent initialize response");
    
    bridge.initialized = true;
    info!("Bridge initialized");
    
    bridge.tools.clear();
    bridge.tool_service_map.clear();
    bridge.collected_servers.clear();
    bridge.tools_collected = false;
    
    for (server_name, stdin) in &mut bridge.processes_stdin {
        let tools_request = json!({
            "jsonrpc": "2.0",
            "id": format!("tools-list-{server_name}"),
            "method": "tools/list"
        });
        
        let message = tools_request.to_string() + "\n";
        stdin.write_all(message.as_bytes()).await?;
        debug!("Sent tools/list request to server: {server_name}");
    }
    
    Ok(())
}

async fn handle_tools_list_request(bridge: &mut Bridge, msg: Value) -> Result<()> {
    info!("Handling tools/list request");
    
    bridge.pending_tools_list_request = Some(msg);
    
    if bridge.tools_collected {
        reply_tools_list(bridge).await?;
    }
    
    Ok(())
}

pub async fn reply_tools_list(bridge: &mut Bridge) -> Result<()> {
    if let Some(request) = bridge.pending_tools_list_request.take() {
        let id = request["id"].clone();
        
        let mut tools_list = Vec::new();
        for (prefixed_name, (_, tool_value)) in &bridge.tools {
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
        
        bridge.broadcast_message(response.to_string()).await?;
        info!("Sent tools/list response with {} tools", tools_list.len());
    }
    
    Ok(())
}

async fn handle_tool_call(bridge: &mut Bridge, msg: Value) -> Result<()> {
    let id = msg["id"].clone();
    let method = msg["method"].as_str().unwrap_or("");
    let params = msg["params"].as_object();
    
    let prefixed_tool_name = params.and_then(|p| p["name"].as_str()).unwrap_or("");
    let arguments = params.and_then(|p| p["arguments"].as_object());
    
    info!("Handling tool call: {prefixed_tool_name} with id {id}");
    
    if let Some((server_name, original_tool_name)) = super::get_original_tool_name(bridge, prefixed_tool_name) {
        if let Some(stdin) = bridge.processes_stdin.get_mut(&server_name) {
            let request = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": {
                    "name": original_tool_name,
                    "arguments": arguments
                }
            });
            
            let message = request.to_string() + "\n";
            stdin.write_all(message.as_bytes()).await?;
            info!("Forwarded tool call to server: {server_name} (original: {original_tool_name})");
            return Ok(());
        }
    }
    
    let error_response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": format!("Tool not found: {prefixed_tool_name}")
        }
    });
    
    bridge.broadcast_message(error_response.to_string()).await?;
    warn!("Tool not found: {prefixed_tool_name}");
    
    Ok(())
}
