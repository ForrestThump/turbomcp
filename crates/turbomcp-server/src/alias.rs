//! Dynamic tool aliasing via config file.
//!
//! This module lets an implementing MCP server define tool aliases in a config
//! file at startup — no recompilation required. Each alias appears to the model
//! as a first-class tool; calling it through its alias name is completely opaque.
//! The underlying tool remains accessible to the model unless hidden via a
//! separate layer such as [`VisibilityLayer`](crate::VisibilityLayer).
//!
//! # How it works
//!
//! An alias maps a new tool name to an existing tool, optionally pre-filling
//! some arguments. At construction time, [`AliasLayer`] calls `list_tools()` on
//! the inner handler, strips the pre-filled keys from the schema of each aliased
//! tool, and caches the resulting [`Tool`] definitions. The model only ever sees
//! the alias name and the reduced schema.
//!
//! When the model calls an alias, [`AliasLayer`] injects the preset arguments
//! (preset values always win over model-provided values) and forwards the call
//! to the real tool under its original name.
//!
//! # Composition order with `VisibilityLayer`
//!
//! If you want an alias to serve as the public face of a hidden tool — i.e.
//! the model should see `show_listings` but NOT `search_where` — compose the
//! layers with `VisibilityLayer` on the **outside**:
//!
//! ```rust,ignore
//! // Correct: VisibilityLayer filters the AliasLayer's output.
//! // show_listings is visible; search_where is hidden.
//! let handler = VisibilityLayer::new(
//!     AliasLayer::new(inner, alias_config)
//! ).disable_tags(["internal"]);
//! ```
//!
//! If instead `AliasLayer` is on the outside, it will call the hidden tool by
//! its original name through the inner `VisibilityLayer`, which will reject the
//! call with `tool_not_found`.
//!
//! # Example config (TOML)
//!
//! ```toml
//! [[aliases]]
//! name = "show_listings"
//! tool = "search_where"
//! description = "Find all active property listings"
//!
//! [aliases.preset_args]
//! front_matter = "listing"
//!
//! [[aliases]]
//! name = "find_expired"
//! tool = "search_where"
//! description = "Find all expired listings"
//!
//! [aliases.preset_args]
//! front_matter = "listing"
//! status = "expired"
//! ```
//!
//! # Example usage
//!
//! ```rust,ignore
//! use turbomcp_server::alias::{AliasConfig, AliasLayer};
//!
//! let config = AliasConfig::from_file("server-config.toml")?;
//! let handler = AliasLayer::new(MyServer::new(), config);
//! handler.serve().await?;
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turbomcp_core::{
    context::RequestContext,
    error::McpResult,
    handler::McpHandler,
};
use turbomcp_types::{
    Prompt, PromptResult, PromptsCapabilities, Resource, ResourceResult, ResourcesCapabilities,
    ResourceTemplate, ServerCapabilities, ServerInfo, Tool, ToolInputSchema, ToolResult,
    ToolsCapabilities,
};

/// A single tool alias definition.
///
/// Aliases are typically loaded from a TOML config file via [`AliasConfig`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alias {
    /// The tool name the model will see and call.
    pub name: String,
    /// The underlying tool to invoke.
    pub tool: String,
    /// Description shown to the model. If absent, inherits from the underlying tool.
    #[serde(default)]
    pub description: Option<String>,
    /// Human-readable title. If absent, inherits from the underlying tool.
    #[serde(default)]
    pub title: Option<String>,
    /// Arguments that are always pre-filled and hidden from the model's schema.
    ///
    /// These are injected at call time and always win over any model-provided
    /// value for the same key.
    #[serde(default)]
    pub preset_args: HashMap<String, Value>,
}

/// Top-level alias config, parsed from an implementing server's config file.
///
/// # TOML format
///
/// ```toml
/// [[aliases]]
/// name    = "show_listings"
/// tool    = "search_where"
/// description = "Find all property listings"
///
/// [aliases.preset_args]
/// front_matter = "listing"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AliasConfig {
    /// The list of aliases to register.
    #[serde(default)]
    pub aliases: Vec<Alias>,
}

impl AliasConfig {
    /// Parse an [`AliasConfig`] from a TOML string.
    ///
    /// # Errors
    ///
    /// Returns a [`toml::de::Error`] if the string is not valid TOML or does
    /// not match the expected structure.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Load an [`AliasConfig`] from a TOML file.
    ///
    /// # Errors
    ///
    /// Returns [`AliasConfigError::Io`] if the file cannot be read, or
    /// [`AliasConfigError::Toml`] if the file content is invalid.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, AliasConfigError> {
        let contents = std::fs::read_to_string(path).map_err(AliasConfigError::Io)?;
        Self::from_toml(&contents).map_err(AliasConfigError::Toml)
    }
}

/// Error returned when loading or parsing an [`AliasConfig`].
#[derive(Debug, thiserror::Error)]
pub enum AliasConfigError {
    /// The config file could not be read from disk.
    #[error("failed to read alias config file: {0}")]
    Io(#[from] std::io::Error),
    /// The config file content could not be parsed as TOML.
    #[error("failed to parse alias config: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Builds the `Tool` definition the model sees for an alias.
///
/// Takes the underlying tool's full definition, strips any pre-filled keys
/// from the schema, and applies the alias's name and optional description/title.
fn build_alias_tool(alias: &Alias, underlying: &Tool) -> Tool {
    Tool {
        name: alias.name.clone(),
        description: alias
            .description
            .clone()
            .or_else(|| underlying.description.clone()),
        title: alias.title.clone().or_else(|| underlying.title.clone()),
        input_schema: strip_preset_args(&underlying.input_schema, &alias.preset_args),
        icons: underlying.icons.clone(),
        annotations: underlying.annotations.clone(),
        execution: underlying.execution.clone(),
        output_schema: underlying.output_schema.clone(),
        // Don't leak internal _meta from the underlying tool.
        meta: None,
    }
}

/// Returns a copy of `schema` with all keys that appear in `preset_args`
/// removed from `properties` and `required`.
///
/// If removing the preset keys empties `properties` or `required`, those
/// fields are set to `None`, which produces a zero-argument schema.
///
/// Note: `extra_keywords` (e.g. `$defs`, `allOf`) is not modified. Tools
/// using JSON Schema composition keywords stored there may still reference
/// preset fields in their metadata, but the security boundary is preserved
/// at call time: preset values always win regardless of what the model sends.
fn strip_preset_args(
    schema: &ToolInputSchema,
    preset_args: &HashMap<String, Value>,
) -> ToolInputSchema {
    if preset_args.is_empty() {
        return schema.clone();
    }

    let mut schema = schema.clone();

    if let Some(Value::Object(ref mut props)) = schema.properties {
        for key in preset_args.keys() {
            props.remove(key);
        }
        if props.is_empty() {
            schema.properties = None;
        }
    }

    if let Some(ref mut required) = schema.required {
        required.retain(|k| !preset_args.contains_key(k));
        if required.is_empty() {
            schema.required = None;
        }
    }

    schema
}

/// Wraps any [`McpHandler`] and injects tool aliases defined in an [`AliasConfig`].
///
/// Each alias appears to the model as an independent tool with a clean schema
/// (pre-filled arguments are stripped out). If all of a tool's parameters are
/// pre-filled, the alias takes no arguments at all.
///
/// **Construction-time behaviour:**
/// - Aliases that reference an unknown underlying tool are skipped with a `warn` log.
/// - A second alias definition with the same name as an earlier one is skipped with a `warn` log.
/// - If an alias name matches an existing inner tool name, a `warn` is logged and the
///   inner tool is hidden from the listing — the alias definition takes its place.
///
/// **All `McpHandler` methods are delegated** to the inner handler, so lifecycle
/// hooks, completions, subscriptions, task management and logging are transparently
/// forwarded.
///
/// # Composition order
///
/// See the [module-level documentation](self) for guidance on composing this
/// layer correctly with [`VisibilityLayer`](crate::VisibilityLayer).
///
/// # Example
///
/// ```rust,ignore
/// let config = AliasConfig::from_file("server.toml")?;
/// let handler = AliasLayer::new(MyServer::new(), config);
/// handler.serve().await?;
/// ```
#[derive(Clone)]
pub struct AliasLayer<H> {
    inner: H,
    /// Map from alias name → Alias definition for O(1) call dispatch.
    alias_map: HashMap<String, Alias>,
    /// Pre-merged tool list: filtered inner tools followed by alias tools.
    /// Built once at construction; `list_tools()` clones this directly.
    tools: Vec<Tool>,
}

impl<H: McpHandler> AliasLayer<H> {
    /// Create a new alias layer, resolving aliases against the inner handler.
    ///
    /// Calls `inner.list_tools()` once at construction to build the alias
    /// definitions. See the struct-level docs for construction-time warnings.
    pub fn new(inner: H, config: AliasConfig) -> Self {
        let inner_tools = inner.list_tools();
        let hint = config.aliases.len();
        let mut alias_map: HashMap<String, Alias> = HashMap::with_capacity(hint);
        let mut alias_tools: Vec<Tool> = Vec::with_capacity(hint);

        for alias in config.aliases {
            // Skip duplicate alias names — first definition wins.
            if alias_map.contains_key(&alias.name) {
                tracing::warn!(
                    alias = %alias.name,
                    "duplicate alias name; skipping second definition"
                );
                continue;
            }

            // Warn when an alias name shadows an existing inner tool.
            if inner_tools.iter().any(|t| t.name == alias.name) {
                tracing::warn!(
                    alias = %alias.name,
                    "alias name shadows an inner tool; the inner tool will be hidden from listings"
                );
            }

            match inner_tools.iter().find(|t| t.name == alias.tool) {
                Some(underlying) => {
                    let tool = build_alias_tool(&alias, underlying);
                    alias_tools.push(tool);
                    alias_map.insert(alias.name.clone(), alias);
                }
                None => {
                    tracing::warn!(
                        alias = %alias.name,
                        underlying_tool = %alias.tool,
                        "alias references unknown tool and will be skipped"
                    );
                }
            }
        }

        // Merge into a single pre-built list: inner tools (minus any shadowed by an alias)
        // followed by the alias tools. This is cloned on every list_tools() call, so building
        // it once here avoids re-filtering and re-allocating on each invocation.
        let tools: Vec<Tool> = inner_tools
            .into_iter()
            .filter(|t| !alias_map.contains_key(&t.name))
            .chain(alias_tools)
            .collect();

        Self {
            inner,
            alias_map,
            tools,
        }
    }

    /// Get a reference to the inner handler.
    pub fn inner(&self) -> &H {
        &self.inner
    }

    /// Consume the layer and return the inner handler.
    pub fn into_inner(self) -> H {
        self.inner
    }

    /// Returns the number of successfully resolved aliases.
    pub fn alias_count(&self) -> usize {
        self.alias_map.len()
    }
}

impl<H: std::fmt::Debug> std::fmt::Debug for AliasLayer<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AliasLayer")
            .field("inner", &self.inner)
            .field("alias_count", &self.alias_map.len())
            .finish()
    }
}

#[allow(clippy::manual_async_fn)]
impl<H: McpHandler> McpHandler for AliasLayer<H> {
    fn server_info(&self) -> ServerInfo {
        self.inner.server_info()
    }

    /// Returns the server's capabilities, merging the inner handler's declared
    /// capabilities with the listing-based capabilities derived from the aliased
    /// tool list. This ensures non-listing capabilities (logging, completions,
    /// subscriptions, tasks) advertised by the inner handler are preserved.
    fn server_capabilities(&self) -> ServerCapabilities {
        let mut caps = self.inner.server_capabilities();

        caps.tools = if !self.tools.is_empty() {
            Some(ToolsCapabilities {
                list_changed: Some(true),
            })
        } else {
            None
        };

        // Preserve the inner handler's subscribe flag when re-deriving resource caps.
        let inner_subscribe = caps.resources.as_ref().and_then(|r| r.subscribe);
        caps.resources = if !self.list_resources().is_empty()
            || !self.list_resource_templates().is_empty()
        {
            Some(ResourcesCapabilities {
                subscribe: inner_subscribe,
                list_changed: Some(true),
            })
        } else {
            None
        };

        caps.prompts = if !self.list_prompts().is_empty() {
            Some(PromptsCapabilities {
                list_changed: Some(true),
            })
        } else {
            None
        };

        caps
    }

    fn list_tools(&self) -> Vec<Tool> {
        self.tools.clone()
    }

    fn list_resources(&self) -> Vec<Resource> {
        self.inner.list_resources()
    }

    fn list_resource_templates(&self) -> Vec<ResourceTemplate> {
        self.inner.list_resource_templates()
    }

    fn list_prompts(&self) -> Vec<Prompt> {
        self.inner.list_prompts()
    }

    fn call_tool<'a>(
        &'a self,
        name: &'a str,
        args: Value,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<ToolResult>>
    + turbomcp_core::marker::MaybeSend
    + 'a {
        // O(1) alias lookup. `alias_ref` borrows from `self.alias_map` with
        // lifetime 'a, valid for the entire returned future.
        let alias_ref = self.alias_map.get(name);

        async move {
            if let Some(alias) = alias_ref {
                // Convert model-provided args to a map; non-object values (including
                // null, which some MCP clients send for zero-arg tools) are treated
                // as an empty map. Preset args are then merged on top, overwriting
                // any model-provided value for the same key.
                let mut obj = match args {
                    Value::Object(m) => m,
                    Value::Null => serde_json::Map::new(),
                    _ => {
                        tracing::debug!(
                            alias = %alias.name,
                            "alias received non-object args; treating as empty"
                        );
                        serde_json::Map::new()
                    }
                };
                for (k, v) in &alias.preset_args {
                    obj.insert(k.clone(), v.clone());
                }
                self.inner
                    .call_tool(&alias.tool, Value::Object(obj), ctx)
                    .await
            } else {
                self.inner.call_tool(name, args, ctx).await
            }
        }
    }

    fn read_resource<'a>(
        &'a self,
        uri: &'a str,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<ResourceResult>>
    + turbomcp_core::marker::MaybeSend
    + 'a {
        self.inner.read_resource(uri, ctx)
    }

    fn get_prompt<'a>(
        &'a self,
        name: &'a str,
        args: Option<Value>,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<PromptResult>>
    + turbomcp_core::marker::MaybeSend
    + 'a {
        self.inner.get_prompt(name, args, ctx)
    }

    // ===== Delegate all remaining trait methods to the inner handler =====

    fn on_initialize(&self) -> impl std::future::Future<Output = McpResult<()>> + turbomcp_core::marker::MaybeSend {
        self.inner.on_initialize()
    }

    fn on_shutdown(&self) -> impl std::future::Future<Output = McpResult<()>> + turbomcp_core::marker::MaybeSend {
        self.inner.on_shutdown()
    }

    fn complete<'a>(
        &'a self,
        params: Value,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<Value>>
    + turbomcp_core::marker::MaybeSend
    + 'a {
        self.inner.complete(params, ctx)
    }

    fn subscribe<'a>(
        &'a self,
        uri: &'a str,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<()>>
    + turbomcp_core::marker::MaybeSend
    + 'a {
        self.inner.subscribe(uri, ctx)
    }

    fn unsubscribe<'a>(
        &'a self,
        uri: &'a str,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<()>>
    + turbomcp_core::marker::MaybeSend
    + 'a {
        self.inner.unsubscribe(uri, ctx)
    }

    fn set_log_level<'a>(
        &'a self,
        level: &'a str,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<()>>
    + turbomcp_core::marker::MaybeSend
    + 'a {
        self.inner.set_log_level(level, ctx)
    }

    fn list_tasks<'a>(
        &'a self,
        cursor: Option<&'a str>,
        limit: Option<usize>,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<turbomcp_types::ListTasksResult>>
    + turbomcp_core::marker::MaybeSend
    + 'a {
        self.inner.list_tasks(cursor, limit, ctx)
    }

    fn get_task<'a>(
        &'a self,
        task_id: &'a str,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<turbomcp_types::Task>>
    + turbomcp_core::marker::MaybeSend
    + 'a {
        self.inner.get_task(task_id, ctx)
    }

    fn cancel_task<'a>(
        &'a self,
        task_id: &'a str,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<turbomcp_types::Task>>
    + turbomcp_core::marker::MaybeSend
    + 'a {
        self.inner.cancel_task(task_id, ctx)
    }

    fn get_task_result<'a>(
        &'a self,
        task_id: &'a str,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<Value>>
    + turbomcp_core::marker::MaybeSend
    + 'a {
        self.inner.get_task_result(task_id, ctx)
    }
}

#[cfg(test)]
#[allow(clippy::manual_async_fn)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use turbomcp_core::{error::McpError, marker::MaybeSend};
    use turbomcp_types::{PromptResult, ResourceResult, ServerInfo, ToolResult};

    // --- minimal test handler ---

    #[derive(Clone, Debug)]
    struct TestHandler;

    impl McpHandler for TestHandler {
        fn server_info(&self) -> ServerInfo {
            ServerInfo::new("test", "1.0.0")
        }

        fn list_tools(&self) -> Vec<Tool> {
            vec![
                Tool {
                    name: "search_where".to_string(),
                    description: Some("Search with filters".to_string()),
                    input_schema: ToolInputSchema::default()
                        .add_property(
                            "front_matter",
                            serde_json::json!({"type": "string", "description": "content type"}),
                        )
                        .require_property("front_matter")
                        .add_property(
                            "limit",
                            serde_json::json!({"type": "integer", "description": "max results"}),
                        ),
                    ..Default::default()
                },
                Tool {
                    name: "no_params".to_string(),
                    description: Some("A tool with no params".to_string()),
                    ..Default::default()
                },
            ]
        }

        fn list_resources(&self) -> Vec<Resource> {
            vec![]
        }

        fn list_prompts(&self) -> Vec<Prompt> {
            vec![]
        }

        fn call_tool<'a>(
            &'a self,
            name: &'a str,
            args: Value,
            _ctx: &'a RequestContext,
        ) -> impl Future<Output = McpResult<ToolResult>> + MaybeSend + 'a {
            let response = format!("{}:{}", name, args);
            async move { Ok(ToolResult::text(response)) }
        }

        fn read_resource<'a>(
            &'a self,
            _uri: &'a str,
            _ctx: &'a RequestContext,
        ) -> impl Future<Output = McpResult<ResourceResult>> + MaybeSend + 'a {
            async move { Err(McpError::resource_not_found("none")) }
        }

        fn get_prompt<'a>(
            &'a self,
            _name: &'a str,
            _args: Option<Value>,
            _ctx: &'a RequestContext,
        ) -> impl Future<Output = McpResult<PromptResult>> + MaybeSend + 'a {
            async move { Err(McpError::prompt_not_found("none")) }
        }
    }

    // --- lifecycle-tracking handler ---

    #[derive(Clone, Debug)]
    struct TrackingHandler {
        initialized: Arc<AtomicBool>,
        shutdown_called: Arc<AtomicBool>,
    }

    impl TrackingHandler {
        fn new() -> Self {
            Self {
                initialized: Arc::new(AtomicBool::new(false)),
                shutdown_called: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl McpHandler for TrackingHandler {
        fn server_info(&self) -> ServerInfo {
            ServerInfo::new("tracking", "1.0.0")
        }
        fn list_tools(&self) -> Vec<Tool> {
            vec![]
        }
        fn list_resources(&self) -> Vec<Resource> {
            vec![]
        }
        fn list_prompts(&self) -> Vec<Prompt> {
            vec![]
        }
        fn call_tool<'a>(
            &'a self,
            name: &'a str,
            _args: Value,
            _ctx: &'a RequestContext,
        ) -> impl Future<Output = McpResult<ToolResult>> + MaybeSend + 'a {
            let name = name.to_string();
            async move { Err(McpError::tool_not_found(&name)) }
        }
        fn read_resource<'a>(
            &'a self,
            _uri: &'a str,
            _ctx: &'a RequestContext,
        ) -> impl Future<Output = McpResult<ResourceResult>> + MaybeSend + 'a {
            async move { Err(McpError::resource_not_found("none")) }
        }
        fn get_prompt<'a>(
            &'a self,
            _name: &'a str,
            _args: Option<Value>,
            _ctx: &'a RequestContext,
        ) -> impl Future<Output = McpResult<PromptResult>> + MaybeSend + 'a {
            async move { Err(McpError::prompt_not_found("none")) }
        }
        fn on_initialize(&self) -> impl Future<Output = McpResult<()>> + MaybeSend {
            let flag = Arc::clone(&self.initialized);
            async move {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            }
        }
        fn on_shutdown(&self) -> impl Future<Output = McpResult<()>> + MaybeSend {
            let flag = Arc::clone(&self.shutdown_called);
            async move {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            }
        }
    }

    // --- helpers ---

    fn make_config(aliases: Vec<Alias>) -> AliasConfig {
        AliasConfig { aliases }
    }

    fn make_alias(name: &str, tool: &str, preset_args: HashMap<String, Value>) -> Alias {
        Alias {
            name: name.to_string(),
            tool: tool.to_string(),
            description: None,
            title: None,
            preset_args,
        }
    }

    // --- schema stripping ---

    #[test]
    fn strip_removes_preset_keys_from_properties_and_required() {
        let schema = ToolInputSchema::default()
            .add_property("front_matter", serde_json::json!({"type": "string"}))
            .require_property("front_matter")
            .add_property("limit", serde_json::json!({"type": "integer"}));

        let preset =
            HashMap::from([("front_matter".to_string(), Value::String("listing".into()))]);
        let stripped = strip_preset_args(&schema, &preset);

        let props = stripped.properties_as_object().unwrap();
        assert!(!props.contains_key("front_matter"), "preset key should be gone");
        assert!(props.contains_key("limit"), "non-preset key should remain");
        assert!(
            stripped
                .required
                .as_ref()
                .map_or(true, |r| !r.contains(&"front_matter".to_string())),
            "preset key should not be required"
        );
    }

    #[test]
    fn strip_all_params_yields_no_properties_and_no_required() {
        let schema = ToolInputSchema::default()
            .add_property("front_matter", serde_json::json!({"type": "string"}))
            .require_property("front_matter");

        let preset =
            HashMap::from([("front_matter".to_string(), Value::String("listing".into()))]);
        let stripped = strip_preset_args(&schema, &preset);

        assert!(stripped.properties.is_none(), "no properties after full strip");
        assert!(stripped.required.is_none(), "no required after full strip");
    }

    #[test]
    fn strip_with_empty_preset_is_a_no_op() {
        let schema = ToolInputSchema::default()
            .add_property("x", serde_json::json!({"type": "string"}));
        let before = schema.clone();
        let after = strip_preset_args(&schema, &HashMap::new());
        assert_eq!(before, after);
    }

    // --- list_tools ---

    #[test]
    fn alias_tool_appears_in_list_tools() {
        let preset =
            HashMap::from([("front_matter".to_string(), Value::String("listing".into()))]);
        let layer = AliasLayer::new(
            TestHandler,
            make_config(vec![make_alias("show_listings", "search_where", preset)]),
        );

        let names: Vec<_> = layer.list_tools().into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"show_listings".to_string()));
        assert!(
            names.contains(&"search_where".to_string()),
            "underlying tool still present when alias has a different name"
        );
    }

    #[test]
    fn alias_schema_does_not_expose_preset_key() {
        let preset =
            HashMap::from([("front_matter".to_string(), Value::String("listing".into()))]);
        let layer = AliasLayer::new(
            TestHandler,
            make_config(vec![make_alias("show_listings", "search_where", preset)]),
        );

        let alias_tool = layer
            .list_tools()
            .into_iter()
            .find(|t| t.name == "show_listings")
            .unwrap();

        let props = alias_tool.input_schema.properties_as_object();
        let has_front_matter = props.map_or(false, |p| p.contains_key("front_matter"));
        assert!(!has_front_matter, "preset arg must not appear in the alias schema");
    }

    #[test]
    fn alias_schema_exposes_remaining_non_preset_params() {
        let preset =
            HashMap::from([("front_matter".to_string(), Value::String("listing".into()))]);
        let layer = AliasLayer::new(
            TestHandler,
            make_config(vec![make_alias("show_listings", "search_where", preset)]),
        );

        let alias_tool = layer
            .list_tools()
            .into_iter()
            .find(|t| t.name == "show_listings")
            .unwrap();

        let props = alias_tool.input_schema.properties_as_object().unwrap();
        assert!(props.contains_key("limit"), "non-preset param must remain visible");
    }

    #[test]
    fn fully_preset_alias_has_no_schema_params() {
        // use search_where with both keys preset so nothing remains
        let preset_full = HashMap::from([
            ("front_matter".to_string(), Value::String("listing".into())),
            ("limit".to_string(), serde_json::json!(10)),
        ]);
        let layer = AliasLayer::new(
            TestHandler,
            make_config(vec![make_alias("all_listings", "search_where", preset_full)]),
        );

        let alias_tool = layer
            .list_tools()
            .into_iter()
            .find(|t| t.name == "all_listings")
            .unwrap();

        assert!(
            alias_tool.input_schema.properties.is_none(),
            "fully-preset alias should have no schema params"
        );
    }

    #[test]
    fn unknown_underlying_tool_is_skipped() {
        let layer = AliasLayer::new(
            TestHandler,
            make_config(vec![make_alias("ghost", "nonexistent_tool", HashMap::new())]),
        );

        assert_eq!(layer.alias_count(), 0);
        let names: Vec<_> = layer.list_tools().into_iter().map(|t| t.name).collect();
        assert!(!names.contains(&"ghost".to_string()));
    }

    #[test]
    fn duplicate_alias_name_skips_second_definition() {
        let aliases = vec![
            make_alias("show_listings", "search_where", HashMap::new()),
            make_alias("show_listings", "no_params", HashMap::new()),
        ];
        let layer = AliasLayer::new(TestHandler, make_config(aliases));

        assert_eq!(layer.alias_count(), 1, "second definition with same name must be skipped");
        let alias_tool = layer
            .list_tools()
            .into_iter()
            .find(|t| t.name == "show_listings")
            .unwrap();
        // First definition (pointing to search_where) wins.
        assert_eq!(
            alias_tool.description.as_deref(),
            Some("Search with filters"),
            "first alias definition must win"
        );
    }

    #[test]
    fn alias_shadowing_inner_tool_deduplicates_in_listing() {
        // Alias has the same name as an inner tool — used to replace the tool
        // with a preset-bound version while keeping the same visible name.
        let preset =
            HashMap::from([("front_matter".to_string(), Value::String("listing".into()))]);
        let layer = AliasLayer::new(
            TestHandler,
            make_config(vec![Alias {
                name: "search_where".to_string(), // same name as inner tool
                tool: "search_where".to_string(),
                description: Some("Preset search".to_string()),
                title: None,
                preset_args: preset,
            }]),
        );

        let tools = layer.list_tools();
        let matches: Vec<_> = tools.iter().filter(|t| t.name == "search_where").collect();
        assert_eq!(matches.len(), 1, "no duplicate names in listing");
        assert_eq!(
            matches[0].description.as_deref(),
            Some("Preset search"),
            "alias description wins over inner tool"
        );
    }

    // --- call_tool dispatch ---

    #[tokio::test]
    async fn alias_call_injects_preset_args() {
        let preset =
            HashMap::from([("front_matter".to_string(), Value::String("listing".into()))]);
        let layer = AliasLayer::new(
            TestHandler,
            make_config(vec![make_alias("show_listings", "search_where", preset)]),
        );

        let ctx = RequestContext::default();
        let result = layer
            .call_tool("show_listings", serde_json::json!({}), &ctx)
            .await
            .unwrap();

        let text = result.first_text().unwrap();
        assert!(text.contains("search_where"), "must be dispatched to underlying tool");
        assert!(text.contains("listing"), "preset value must appear in forwarded args");
    }

    #[tokio::test]
    async fn preset_args_override_model_provided_values() {
        let preset =
            HashMap::from([("front_matter".to_string(), Value::String("listing".into()))]);
        let layer = AliasLayer::new(
            TestHandler,
            make_config(vec![make_alias("show_listings", "search_where", preset)]),
        );

        let ctx = RequestContext::default();
        let result = layer
            .call_tool(
                "show_listings",
                serde_json::json!({"front_matter": "blog", "limit": 5}),
                &ctx,
            )
            .await
            .unwrap();

        let text = result.first_text().unwrap();
        assert!(text.contains("listing"), "preset must win over model value");
        assert!(
            !text.contains("\"front_matter\":\"blog\""),
            "model value must be overwritten"
        );
    }

    #[tokio::test]
    async fn non_alias_call_passes_through_unchanged() {
        let layer = AliasLayer::new(TestHandler, AliasConfig::default());

        let ctx = RequestContext::default();
        let result = layer
            .call_tool("search_where", serde_json::json!({"front_matter": "blog"}), &ctx)
            .await
            .unwrap();

        let text = result.first_text().unwrap();
        assert!(text.contains("search_where"));
        assert!(text.contains("blog"));
    }

    #[tokio::test]
    async fn null_args_to_alias_treated_as_empty_with_presets_injected() {
        let preset =
            HashMap::from([("front_matter".to_string(), Value::String("listing".into()))]);
        let layer = AliasLayer::new(
            TestHandler,
            make_config(vec![make_alias("show_listings", "search_where", preset)]),
        );

        let ctx = RequestContext::default();
        // Some MCP clients send null instead of {} for zero-arg or no-extra-arg calls.
        let result = layer
            .call_tool("show_listings", Value::Null, &ctx)
            .await
            .unwrap();

        let text = result.first_text().unwrap();
        assert!(text.contains("search_where"), "must reach the underlying tool");
        assert!(text.contains("listing"), "preset must still be injected when args is null");
    }

    // --- description / title inheritance ---

    #[test]
    fn alias_inherits_description_when_not_overridden() {
        let layer = AliasLayer::new(
            TestHandler,
            make_config(vec![Alias {
                name: "show_listings".to_string(),
                tool: "search_where".to_string(),
                description: None,
                title: None,
                preset_args: HashMap::new(),
            }]),
        );

        let alias_tool = layer
            .list_tools()
            .into_iter()
            .find(|t| t.name == "show_listings")
            .unwrap();

        assert_eq!(alias_tool.description.as_deref(), Some("Search with filters"));
    }

    #[test]
    fn alias_description_overrides_underlying() {
        let layer = AliasLayer::new(
            TestHandler,
            make_config(vec![Alias {
                name: "show_listings".to_string(),
                tool: "search_where".to_string(),
                description: Some("Find all property listings".to_string()),
                title: None,
                preset_args: HashMap::new(),
            }]),
        );

        let alias_tool = layer
            .list_tools()
            .into_iter()
            .find(|t| t.name == "show_listings")
            .unwrap();

        assert_eq!(
            alias_tool.description.as_deref(),
            Some("Find all property listings")
        );
    }

    // --- lifecycle delegation ---

    #[tokio::test]
    async fn on_initialize_delegates_to_inner_handler() {
        let handler = TrackingHandler::new();
        let initialized = Arc::clone(&handler.initialized);

        let layer = AliasLayer::new(handler, AliasConfig::default());
        layer.on_initialize().await.unwrap();

        assert!(
            initialized.load(Ordering::SeqCst),
            "on_initialize must reach the inner handler"
        );
    }

    #[tokio::test]
    async fn on_shutdown_delegates_to_inner_handler() {
        let handler = TrackingHandler::new();
        let shutdown_called = Arc::clone(&handler.shutdown_called);

        let layer = AliasLayer::new(handler, AliasConfig::default());
        layer.on_shutdown().await.unwrap();

        assert!(
            shutdown_called.load(Ordering::SeqCst),
            "on_shutdown must reach the inner handler"
        );
    }

    // --- TOML parsing ---

    #[test]
    fn parse_alias_config_from_toml() {
        let toml = r#"
[[aliases]]
name = "show_listings"
tool = "search_where"
description = "Find all listings"

[aliases.preset_args]
front_matter = "listing"

[[aliases]]
name = "find_expired"
tool = "search_where"

[aliases.preset_args]
front_matter = "listing"
status = "expired"
"#;
        let config = AliasConfig::from_toml(toml).unwrap();
        assert_eq!(config.aliases.len(), 2);
        assert_eq!(config.aliases[0].name, "show_listings");
        assert_eq!(
            config.aliases[0].preset_args.get("front_matter"),
            Some(&Value::String("listing".into()))
        );
        assert_eq!(config.aliases[1].name, "find_expired");
        assert_eq!(config.aliases[1].preset_args.len(), 2);
    }

    #[test]
    fn empty_aliases_section_is_valid() {
        let config = AliasConfig::from_toml("").unwrap();
        assert_eq!(config.aliases.len(), 0);
    }

    // --- from_file ---

    #[test]
    fn from_file_loads_valid_config() {
        let content = r#"
[[aliases]]
name = "show_listings"
tool = "search_where"
description = "Find listings"
"#;
        let path = std::env::temp_dir().join("turbomcp_alias_test_load.toml");
        std::fs::write(&path, content).unwrap();
        let result = AliasConfig::from_file(&path);
        let _ = std::fs::remove_file(&path);

        let config = result.unwrap();
        assert_eq!(config.aliases.len(), 1);
        assert_eq!(config.aliases[0].name, "show_listings");
    }

    #[test]
    fn from_file_returns_io_error_for_missing_file() {
        let result = AliasConfig::from_file("/this/path/does/not/exist/turbomcp.toml");
        assert!(
            matches!(result, Err(AliasConfigError::Io(_))),
            "missing file must return Io error"
        );
    }

    #[test]
    fn from_file_returns_toml_error_for_malformed_content() {
        let path = std::env::temp_dir().join("turbomcp_alias_test_bad.toml");
        std::fs::write(&path, "[[aliases\nthis is not valid toml!!!").unwrap();
        let result = AliasConfig::from_file(&path);
        let _ = std::fs::remove_file(&path);
        assert!(
            matches!(result, Err(AliasConfigError::Toml(_))),
            "malformed TOML must return Toml error"
        );
    }

    #[test]
    fn server_capabilities_inherits_non_listing_caps_from_inner() {
        use turbomcp_types::ServerCapabilities;

        // Build an inner handler that exposes a tool, so server_capabilities()
        // will set caps.tools. We then verify that the AliasLayer does not zero
        // out unrelated capability fields that the default McpHandler derives.
        let layer = AliasLayer::new(TestHandler, AliasConfig::default());
        let caps: ServerCapabilities = layer.server_capabilities();

        // TestHandler has tools, so tools capability must be present.
        assert!(
            caps.tools.is_some(),
            "tools capability must be set when inner handler has tools"
        );

        // Inner handler has no resources or prompts; those fields should be absent
        // rather than erroneously populated.
        assert!(
            caps.resources.is_none(),
            "resources capability must not be set when inner has no resources"
        );
        assert!(
            caps.prompts.is_none(),
            "prompts capability must not be set when inner has no prompts"
        );
    }
}
