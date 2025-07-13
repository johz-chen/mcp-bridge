use super::core::Bridge;
use anyhow::Result;
use serde_json::Value;
use tokio::time::Instant;
use tracing::{debug, info};

pub async fn handle_process_output(
    bridge: &mut Bridge,
    server_name: &str,
    output: String,
) -> Result<()> {
    debug!(">> Received output from {server_name}: {}", output.trim());

    let msg_value: Value = match serde_json::from_str(&output) {
        Ok(value) => value,
        Err(_) => Value::String(output.clone()),
    };

    let mut should_broadcast = true;

    if let Some(result) = msg_value.get("result") {
        if let Some(id) = msg_value.get("id") {
            if msg_value.get("method").is_none() {
                if let Some(id_str) = id.as_str() {
                    if id_str.starts_with("tools-list-") {
                        should_broadcast = false;

                        if let Some(tools) = result["tools"].as_array() {
                            bridge.collected_servers.insert(server_name.to_string());

                            for tool in tools {
                                if let Some(tool_obj) = tool.as_object() {
                                    if let Some(Value::String(original_tool_name)) =
                                        tool_obj.get("name")
                                    {
                                        let prefixed_name = super::generate_prefixed_tool_name(
                                            bridge,
                                            server_name,
                                            original_tool_name,
                                        );

                                        bridge.tools.insert(
                                            prefixed_name.clone(),
                                            (server_name.to_string(), tool.clone()),
                                        );

                                        bridge.tool_service_map.insert(
                                            prefixed_name,
                                            (server_name.to_string(), original_tool_name.clone()),
                                        );
                                    }
                                }
                            }

                            info!("Collected {} tools from {server_name}", tools.len());

                            if bridge.collected_servers.len() == bridge.config.servers.len() {
                                bridge.tools_collected = true;
                                info!("All tools collected, total: {}", bridge.tools.len());

                                super::message_handler::reply_tools_list(bridge).await?;
                            }
                        }
                    }
                }
            }
        }
    }

    if should_broadcast {
        bridge.broadcast_message(msg_value.to_string()).await?;
    }

    bridge.last_activity = Instant::now();
    Ok(())
}
