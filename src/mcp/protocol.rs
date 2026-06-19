use serde::Deserialize;

/// A tool an MCP server advertises via `tools/list`.
#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: serde_json::Value,
}

/// One content block in a `tools/call` result. MCP defines more block
/// kinds (image, resource, ...); only text is round-tripped today since
/// that covers every Extension call site Open String currently has --
/// unrecognized kinds are dropped rather than failing the whole result.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpContent {
    Text {
        text: String,
    },
    #[serde(other)]
    Unsupported,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpToolResult {
    #[serde(default)]
    pub content: Vec<McpContent>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

impl McpToolResult {
    /// Concatenates every text block in the result, which is the only
    /// content kind Sub Agents/the Mediator currently consume.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                McpContent::Text { text } => Some(text.as_str()),
                McpContent::Unsupported => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("I/O error communicating with MCP server: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON from MCP server: {0}")]
    Json(#[from] serde_json::Error),
    #[error("MCP server closed the connection unexpectedly")]
    Closed,
    #[error("MCP server returned an error: {0}")]
    Rpc(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_joins_only_text_blocks_with_newlines() {
        let result = McpToolResult {
            content: vec![
                McpContent::Text {
                    text: "first".to_string(),
                },
                McpContent::Unsupported,
                McpContent::Text {
                    text: "second".to_string(),
                },
            ],
            is_error: false,
        };
        assert_eq!(result.text(), "first\nsecond");
    }
}
