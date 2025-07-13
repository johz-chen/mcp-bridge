use super::core::Bridge;

pub fn generate_prefixed_tool_name(_bridge: &Bridge, server_name: &str, tool_name: &str) -> String {
    let normalized_server_name = server_name.replace('-', "_");
    format!("{normalized_server_name}_xzcli_{tool_name}")
}

pub fn get_original_tool_name(
    bridge: &Bridge,
    prefixed_tool_name: &str,
) -> Option<(String, String)> {
    bridge.tool_service_map.get(prefixed_tool_name).cloned()
}
