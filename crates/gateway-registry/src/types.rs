/// A downstream tool's identity, as discovered from a backend's `tools/list`
/// (MCP) response or a static config declaration (future HTTP backends).
#[derive(Debug, Clone)]
pub struct ToolEntry {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
