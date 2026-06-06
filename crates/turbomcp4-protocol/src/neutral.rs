//! Version-stable, handler-facing types — the surface user handlers speak.
//!
//! Handlers must not couple to a wire version. They return these neutral types;
//! the `VersionDispatcher` widens them to the *active* version's generated wire
//! type, filling version-specific required fields (`resultType`, `cacheScope`,
//! `ttlMs`, …) with spec defaults the handler shouldn't have to know about.
//!
//! This is the small, deliberately hand-curated subset the plan calls
//! `neutral/` (§3) — distinct from the full per-version generated surface. It
//! grows one method-family at a time as phases land; Phase 2 covers the
//! `tools/*` family and discovery. Because the trait signatures in
//! `turbomcp4-server` are expressed in these types, wiring the second wire
//! version (Phase 5) adds conversions here without changing any handler.
//!
//! Conversions are intentionally one-directional (neutral → wire) and total
//! (`From`, never failing): a handler can always be serialized to the wire.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::{Map, Value};

use crate::v2026_draft::types as draft;

/// A neutral content block. Phase 2 models text; the enum is `#[non_exhaustive]`
/// so image/audio/resource blocks slot in without breaking callers.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Content {
    /// Plain UTF-8 text.
    Text(String),
}

impl Content {
    /// A text content block.
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }
}

/// A tool descriptor.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Tool {
    /// Programmatic identifier (what `tools/call` references).
    pub name: String,
    /// Optional human-facing display name.
    pub title: Option<String>,
    /// Optional natural-language description (a hint to the model).
    pub description: Option<String>,
    /// JSON Schema object describing the tool's arguments
    /// (e.g. `{"type":"object","properties":{…}}`).
    pub input_schema: Value,
}

impl Tool {
    /// A tool with the given name and argument schema; no title/description.
    pub fn new(name: impl Into<String>, input_schema: Value) -> Self {
        Self {
            name: name.into(),
            title: None,
            description: None,
            input_schema,
        }
    }

    /// Set the description (builder style).
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the title (builder style).
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// Result of `tools/list`.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ListToolsResult {
    /// The tools offered.
    pub tools: Vec<Tool>,
    /// Opaque pagination cursor; `Some` means more pages follow.
    pub next_cursor: Option<String>,
}

impl ListToolsResult {
    /// A single-page result over `tools`.
    pub fn new(tools: Vec<Tool>) -> Self {
        Self {
            tools,
            next_cursor: None,
        }
    }
}

/// Decoded `tools/call` arguments (the framework strips wire `_meta` into
/// `RequestContext` before the handler sees this).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CallToolParams {
    /// Name of the tool to invoke.
    pub name: String,
    /// Arguments object (may be empty).
    pub arguments: Map<String, Value>,
}

impl CallToolParams {
    /// Construct from a tool name and arguments object.
    pub fn new(name: impl Into<String>, arguments: Map<String, Value>) -> Self {
        Self {
            name: name.into(),
            arguments,
        }
    }
}

/// Result of `tools/call`. Per spec, tool-level failure is `is_error`, *not* a
/// JSON-RPC error (so the model can see and self-correct).
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct CallToolResult {
    /// Unstructured result content.
    pub content: Vec<Content>,
    /// `true` if the tool itself failed.
    pub is_error: bool,
    /// Optional structured result conforming to the tool's output schema.
    pub structured_content: Option<Value>,
}

impl CallToolResult {
    /// A successful result carrying a single text block.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: alloc::vec![Content::text(s)],
            is_error: false,
            structured_content: None,
        }
    }

    /// A failed result (`is_error = true`) carrying a single text block.
    pub fn error(s: impl Into<String>) -> Self {
        Self {
            content: alloc::vec![Content::text(s)],
            is_error: true,
            structured_content: None,
        }
    }
}

// ---- neutral → DRAFT-2026-v1 wire conversions --------------------------------

impl From<Content> for draft::ContentBlock {
    fn from(c: Content) -> Self {
        match c {
            Content::Text(text) => draft::ContentBlock::TextContent(draft::TextContent {
                annotations: None,
                meta: None,
                text,
                type_: "text".to_string(),
            }),
        }
    }
}

impl From<Tool> for draft::Tool {
    fn from(t: Tool) -> Self {
        // The neutral `input_schema` is a JSON Schema object; deserialize it
        // into the typed wrapper. A non-object or schema-less value falls back
        // to an empty object schema rather than failing the conversion.
        let input_schema =
            serde_json::from_value(t.input_schema).unwrap_or(draft::ToolInputSchema {
                schema: None,
                type_: "object".to_string(),
                extra: Map::new(),
            });
        draft::Tool {
            annotations: None,
            description: t.description,
            icons: Vec::new(),
            input_schema,
            meta: None,
            name: t.name,
            output_schema: None,
            title: t.title,
        }
    }
}

impl From<ListToolsResult> for draft::ListToolsResult {
    fn from(r: ListToolsResult) -> Self {
        draft::ListToolsResult {
            cache_scope: draft::ListToolsResultCacheScope::Private,
            meta: None,
            next_cursor: r.next_cursor,
            result_type: draft::ResultType::Complete,
            tools: r.tools.into_iter().map(Into::into).collect(),
            ttl_ms: 0,
        }
    }
}

impl From<CallToolResult> for draft::CallToolResult {
    fn from(r: CallToolResult) -> Self {
        draft::CallToolResult {
            content: r.content.into_iter().map(Into::into).collect(),
            is_error: Some(r.is_error),
            meta: None,
            result_type: draft::ResultType::Complete,
            structured_content: r.structured_content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_widens_to_draft_wire() {
        let neutral = Tool::new("echo", json!({"type": "object", "properties": {}}))
            .with_description("Echoes input");
        let wire: draft::Tool = neutral.into();
        assert_eq!(wire.name, "echo");
        assert_eq!(wire.input_schema.type_, "object");
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["name"], "echo");
        assert_eq!(v["inputSchema"]["type"], "object");
    }

    #[test]
    fn call_result_carries_result_type_and_is_error() {
        let wire: draft::CallToolResult = CallToolResult::error("boom").into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["resultType"], "complete");
        assert_eq!(v["isError"], true);
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "boom");
    }

    #[test]
    fn list_result_fills_draft_required_fields() {
        let wire: draft::ListToolsResult =
            ListToolsResult::new(alloc::vec![Tool::new("a", json!({"type": "object"}))]).into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["resultType"], "complete");
        assert_eq!(v["cacheScope"], "private");
        assert_eq!(v["ttlMs"], 0);
        assert_eq!(v["tools"][0]["name"], "a");
    }
}
