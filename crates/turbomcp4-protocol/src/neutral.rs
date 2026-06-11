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

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::{Map, Value};

use crate::v2025_11_25::types as legacy;
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

// ---- pagination ---------------------------------------------------------------

/// Inbound parameters shared by every `*/list` method: an opaque pagination
/// cursor (absent on the first page). The framework decodes it from the wire;
/// handlers echo a `next_cursor` in their result to advertise another page.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ListParams {
    /// Cursor returned by a previous page, or `None` for the first page.
    pub cursor: Option<String>,
}

impl ListParams {
    /// First-page request (no cursor).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request continuing from `cursor`.
    #[must_use]
    pub fn with_cursor(cursor: impl Into<String>) -> Self {
        Self {
            cursor: Some(cursor.into()),
        }
    }
}

// ---- resources ----------------------------------------------------------------

/// A resource descriptor (`resources/list`).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Resource {
    /// The resource URI (what `resources/read` references).
    pub uri: String,
    /// Programmatic identifier / fallback display name.
    pub name: String,
    /// Optional human-facing display name.
    pub title: Option<String>,
    /// Optional natural-language description (a hint to the model).
    pub description: Option<String>,
    /// MIME type, if known.
    pub mime_type: Option<String>,
    /// Raw content size in bytes (before any encoding), if known.
    pub size: Option<u64>,
}

impl Resource {
    /// A resource with the given URI and name.
    pub fn new(uri: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            name: name.into(),
            title: None,
            description: None,
            mime_type: None,
            size: None,
        }
    }

    /// Set the title (builder style).
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the description (builder style).
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the MIME type (builder style).
    #[must_use]
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }
}

/// The contents of a read resource (`resources/read`): UTF-8 text or a
/// base64-encoded binary blob. `#[non_exhaustive]` so future kinds slot in.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ResourceContents {
    /// Text contents.
    Text {
        /// URI these contents belong to.
        uri: String,
        /// MIME type, if known.
        mime_type: Option<String>,
        /// The text.
        text: String,
    },
    /// Binary contents, base64-encoded.
    Blob {
        /// URI these contents belong to.
        uri: String,
        /// MIME type, if known.
        mime_type: Option<String>,
        /// Base64-encoded bytes.
        blob: String,
    },
}

impl ResourceContents {
    /// Text contents for `uri` (no MIME type).
    pub fn text(uri: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Text {
            uri: uri.into(),
            mime_type: None,
            text: text.into(),
        }
    }

    /// Base64 binary contents for `uri` (no MIME type).
    pub fn blob(uri: impl Into<String>, blob: impl Into<String>) -> Self {
        Self::Blob {
            uri: uri.into(),
            mime_type: None,
            blob: blob.into(),
        }
    }

    /// Set the MIME type on either variant (builder style).
    #[must_use]
    pub fn with_mime_type(mut self, mime: impl Into<String>) -> Self {
        match &mut self {
            Self::Text { mime_type, .. } | Self::Blob { mime_type, .. } => {
                *mime_type = Some(mime.into());
            }
        }
        self
    }
}

/// Result of `resources/list`.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ListResourcesResult {
    /// The resources offered.
    pub resources: Vec<Resource>,
    /// Opaque pagination cursor; `Some` means more pages follow.
    pub next_cursor: Option<String>,
}

impl ListResourcesResult {
    /// A single-page result over `resources`.
    pub fn new(resources: Vec<Resource>) -> Self {
        Self {
            resources,
            next_cursor: None,
        }
    }
}

/// Result of `resources/read`.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ReadResourceResult {
    /// One or more content items (a single resource may expand to several).
    pub contents: Vec<ResourceContents>,
}

impl ReadResourceResult {
    /// A result carrying the given contents.
    pub fn new(contents: Vec<ResourceContents>) -> Self {
        Self { contents }
    }

    /// A result carrying a single text item.
    pub fn text(uri: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            contents: alloc::vec![ResourceContents::text(uri, text)],
        }
    }
}

/// A resource template (`resources/templates/list`): a URI Template (RFC 6570)
/// describing a family of resources.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ResourceTemplate {
    /// URI Template (e.g. `file://{path}`).
    pub uri_template: String,
    /// Programmatic identifier / fallback display name.
    pub name: String,
    /// Optional human-facing display name.
    pub title: Option<String>,
    /// Optional natural-language description.
    pub description: Option<String>,
    /// MIME type shared by all matching resources, if uniform.
    pub mime_type: Option<String>,
}

impl ResourceTemplate {
    /// A template with the given URI Template and name.
    pub fn new(uri_template: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uri_template: uri_template.into(),
            name: name.into(),
            title: None,
            description: None,
            mime_type: None,
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

    /// Set the MIME type (builder style).
    #[must_use]
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }
}

/// Result of `resources/templates/list`.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ListResourceTemplatesResult {
    /// The templates offered.
    pub resource_templates: Vec<ResourceTemplate>,
    /// Opaque pagination cursor; `Some` means more pages follow.
    pub next_cursor: Option<String>,
}

impl ListResourceTemplatesResult {
    /// A single-page result over `resource_templates`.
    pub fn new(resource_templates: Vec<ResourceTemplate>) -> Self {
        Self {
            resource_templates,
            next_cursor: None,
        }
    }
}

/// `resources/read` parameters (the framework strips wire `_meta` into
/// `RequestContext` before the handler sees this).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ReadResourceParams {
    /// URI of the resource to read.
    pub uri: String,
}

impl ReadResourceParams {
    /// Construct from a URI.
    pub fn new(uri: impl Into<String>) -> Self {
        Self { uri: uri.into() }
    }
}

// ---- prompts ------------------------------------------------------------------

/// Who authored a prompt message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The end user.
    User,
    /// The model.
    Assistant,
}

/// A declared prompt argument (used for templating and completion).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PromptArgument {
    /// Programmatic identifier / fallback display name.
    pub name: String,
    /// Optional human-facing display name.
    pub title: Option<String>,
    /// Optional natural-language description.
    pub description: Option<String>,
    /// Whether the argument must be provided.
    pub required: bool,
}

impl PromptArgument {
    /// An optional argument with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            title: None,
            description: None,
            required: false,
        }
    }

    /// Mark the argument required (builder style).
    #[must_use]
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
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

/// A prompt descriptor (`prompts/list`).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Prompt {
    /// Programmatic identifier (what `prompts/get` references).
    pub name: String,
    /// Optional human-facing display name.
    pub title: Option<String>,
    /// Optional natural-language description.
    pub description: Option<String>,
    /// Declared arguments for templating.
    pub arguments: Vec<PromptArgument>,
}

impl Prompt {
    /// A prompt with the given name and no arguments.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            title: None,
            description: None,
            arguments: Vec::new(),
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

    /// Append a declared argument (builder style).
    #[must_use]
    pub fn with_argument(mut self, argument: PromptArgument) -> Self {
        self.arguments.push(argument);
        self
    }
}

/// A single message in a rendered prompt.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PromptMessage {
    /// Who authored the message.
    pub role: Role,
    /// The message content.
    pub content: Content,
}

impl PromptMessage {
    /// A user-authored message.
    pub fn user(content: Content) -> Self {
        Self {
            role: Role::User,
            content,
        }
    }

    /// An assistant-authored message.
    pub fn assistant(content: Content) -> Self {
        Self {
            role: Role::Assistant,
            content,
        }
    }

    /// A user-authored text message.
    pub fn user_text(text: impl Into<String>) -> Self {
        Self::user(Content::text(text))
    }

    /// An assistant-authored text message.
    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self::assistant(Content::text(text))
    }
}

/// Result of `prompts/list`.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ListPromptsResult {
    /// The prompts offered.
    pub prompts: Vec<Prompt>,
    /// Opaque pagination cursor; `Some` means more pages follow.
    pub next_cursor: Option<String>,
}

impl ListPromptsResult {
    /// A single-page result over `prompts`.
    pub fn new(prompts: Vec<Prompt>) -> Self {
        Self {
            prompts,
            next_cursor: None,
        }
    }
}

/// Result of `prompts/get`: a rendered prompt as a message sequence.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct GetPromptResult {
    /// Optional description of the rendered prompt.
    pub description: Option<String>,
    /// The messages.
    pub messages: Vec<PromptMessage>,
}

impl GetPromptResult {
    /// A result carrying the given messages.
    pub fn new(messages: Vec<PromptMessage>) -> Self {
        Self {
            description: None,
            messages,
        }
    }

    /// Set the description (builder style).
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// `prompts/get` parameters.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct GetPromptParams {
    /// Name of the prompt to render.
    pub name: String,
    /// Templating arguments (string→string per spec).
    pub arguments: BTreeMap<String, String>,
}

impl GetPromptParams {
    /// Construct from a prompt name and arguments.
    pub fn new(name: impl Into<String>, arguments: BTreeMap<String, String>) -> Self {
        Self {
            name: name.into(),
            arguments,
        }
    }
}

// ---- completions --------------------------------------------------------------

/// Result of `completion/complete`: up to 100 suggested values.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct CompleteResult {
    /// Suggested completion values (spec caps at 100).
    pub values: Vec<String>,
    /// Total available, which may exceed `values.len()`.
    pub total: Option<u32>,
    /// Whether more values exist beyond those returned.
    pub has_more: Option<bool>,
}

impl CompleteResult {
    /// A result carrying the given values.
    pub fn new(values: Vec<String>) -> Self {
        Self {
            values,
            total: None,
            has_more: None,
        }
    }

    /// Set the total count (builder style).
    #[must_use]
    pub fn with_total(mut self, total: u32) -> Self {
        self.total = Some(total);
        self
    }

    /// Set the has-more flag (builder style).
    #[must_use]
    pub fn with_has_more(mut self, has_more: bool) -> Self {
        self.has_more = Some(has_more);
        self
    }
}

/// What a completion request is completing against.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum CompletionReference {
    /// An argument of a prompt, by prompt name.
    Prompt {
        /// Prompt name.
        name: String,
    },
    /// A variable of a resource template, by URI (template).
    ResourceTemplate {
        /// Resource URI or URI template.
        uri: String,
    },
}

/// The argument being completed: its name and the partial value typed so far.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CompletionArgument {
    /// Name of the argument.
    pub name: String,
    /// Partial value entered so far.
    pub value: String,
}

impl CompletionArgument {
    /// Construct from a name and partial value.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// `completion/complete` parameters.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CompleteParams {
    /// What is being completed (a prompt or a resource template).
    pub reference: CompletionReference,
    /// The argument and its partial value.
    pub argument: CompletionArgument,
    /// Previously-resolved arguments (for multi-variable templates).
    pub context_arguments: BTreeMap<String, String>,
}

impl CompleteParams {
    /// Construct from a reference and the argument being completed.
    pub fn new(reference: CompletionReference, argument: CompletionArgument) -> Self {
        Self {
            reference,
            argument,
            context_arguments: BTreeMap::new(),
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

// resources

impl From<Resource> for draft::Resource {
    fn from(r: Resource) -> Self {
        draft::Resource {
            annotations: None,
            description: r.description,
            icons: Vec::new(),
            meta: None,
            mime_type: r.mime_type,
            name: r.name,
            size: r.size.map(|s| i64::try_from(s).unwrap_or(i64::MAX)),
            title: r.title,
            uri: r.uri,
        }
    }
}

impl From<ResourceContents> for draft::ReadResourceResultContentsItem {
    fn from(c: ResourceContents) -> Self {
        match c {
            ResourceContents::Text {
                uri,
                mime_type,
                text,
            } => draft::ReadResourceResultContentsItem::TextResourceContents(
                draft::TextResourceContents {
                    meta: None,
                    mime_type,
                    text,
                    uri,
                },
            ),
            ResourceContents::Blob {
                uri,
                mime_type,
                blob,
            } => draft::ReadResourceResultContentsItem::BlobResourceContents(
                draft::BlobResourceContents {
                    blob,
                    meta: None,
                    mime_type,
                    uri,
                },
            ),
        }
    }
}

impl From<ListResourcesResult> for draft::ListResourcesResult {
    fn from(r: ListResourcesResult) -> Self {
        draft::ListResourcesResult {
            cache_scope: draft::ListResourcesResultCacheScope::Private,
            meta: None,
            next_cursor: r.next_cursor,
            resources: r.resources.into_iter().map(Into::into).collect(),
            result_type: draft::ResultType::Complete,
            ttl_ms: 0,
        }
    }
}

impl From<ReadResourceResult> for draft::ReadResourceResult {
    fn from(r: ReadResourceResult) -> Self {
        draft::ReadResourceResult {
            cache_scope: draft::ReadResourceResultCacheScope::Private,
            contents: r.contents.into_iter().map(Into::into).collect(),
            meta: None,
            result_type: draft::ResultType::Complete,
            ttl_ms: 0,
        }
    }
}

impl From<ResourceTemplate> for draft::ResourceTemplate {
    fn from(t: ResourceTemplate) -> Self {
        draft::ResourceTemplate {
            annotations: None,
            description: t.description,
            icons: Vec::new(),
            meta: None,
            mime_type: t.mime_type,
            name: t.name,
            title: t.title,
            uri_template: t.uri_template,
        }
    }
}

impl From<ListResourceTemplatesResult> for draft::ListResourceTemplatesResult {
    fn from(r: ListResourceTemplatesResult) -> Self {
        draft::ListResourceTemplatesResult {
            cache_scope: draft::ListResourceTemplatesResultCacheScope::Private,
            meta: None,
            next_cursor: r.next_cursor,
            resource_templates: r.resource_templates.into_iter().map(Into::into).collect(),
            result_type: draft::ResultType::Complete,
            ttl_ms: 0,
        }
    }
}

// prompts

impl From<Role> for draft::Role {
    fn from(r: Role) -> Self {
        match r {
            Role::User => draft::Role::User,
            Role::Assistant => draft::Role::Assistant,
        }
    }
}

impl From<PromptArgument> for draft::PromptArgument {
    fn from(a: PromptArgument) -> Self {
        draft::PromptArgument {
            description: a.description,
            name: a.name,
            required: Some(a.required),
            title: a.title,
        }
    }
}

impl From<Prompt> for draft::Prompt {
    fn from(p: Prompt) -> Self {
        draft::Prompt {
            arguments: p.arguments.into_iter().map(Into::into).collect(),
            description: p.description,
            icons: Vec::new(),
            meta: None,
            name: p.name,
            title: p.title,
        }
    }
}

impl From<PromptMessage> for draft::PromptMessage {
    fn from(m: PromptMessage) -> Self {
        draft::PromptMessage {
            content: m.content.into(),
            role: m.role.into(),
        }
    }
}

impl From<ListPromptsResult> for draft::ListPromptsResult {
    fn from(r: ListPromptsResult) -> Self {
        draft::ListPromptsResult {
            cache_scope: draft::ListPromptsResultCacheScope::Private,
            meta: None,
            next_cursor: r.next_cursor,
            prompts: r.prompts.into_iter().map(Into::into).collect(),
            result_type: draft::ResultType::Complete,
            ttl_ms: 0,
        }
    }
}

impl From<GetPromptResult> for draft::GetPromptResult {
    fn from(r: GetPromptResult) -> Self {
        draft::GetPromptResult {
            description: r.description,
            messages: r.messages.into_iter().map(Into::into).collect(),
            meta: None,
            result_type: draft::ResultType::Complete,
        }
    }
}

// completions

impl From<CompleteResult> for draft::CompleteResult {
    fn from(r: CompleteResult) -> Self {
        draft::CompleteResult {
            completion: draft::CompleteResultCompletion {
                has_more: r.has_more,
                total: r.total.map(i64::from),
                values: r.values,
            },
            meta: None,
            result_type: draft::ResultType::Complete,
        }
    }
}

// ---- neutral → 2025-11-25 wire conversions ------------------------------------
//
// The legacy mirror of the draft conversions above. Differences from the draft
// wire that these conversions absorb so handlers never see them:
// - no `resultType` / `cacheScope` / `ttlMs` (the draft's caching envelope
//   doesn't exist in 2025-11-25);
// - `_meta` is a plain map (skipped when empty), not an `Option`;
// - `CallToolResult.structuredContent` is an *object* by schema, so a neutral
//   non-object `structured_content` value cannot be represented and is dropped
//   (the draft wire keeps any JSON value — see `From<CallToolResult>` below);
// - `ToolInputSchema` is closed (`properties`/`required`/`$schema`/`type`):
//   any other top-level schema keywords a handler put in `input_schema` are
//   not representable on this wire version and do not survive conversion.

impl From<Content> for legacy::ContentBlock {
    fn from(c: Content) -> Self {
        match c {
            Content::Text(text) => legacy::ContentBlock::TextContent(legacy::TextContent {
                annotations: None,
                meta: Map::new(),
                text,
                type_: "text".to_string(),
            }),
        }
    }
}

impl From<Tool> for legacy::Tool {
    fn from(t: Tool) -> Self {
        // Deserialize the neutral JSON Schema into the (closed) legacy wrapper;
        // a non-object value falls back to an empty object schema. `execution`
        // (task support) is left unset here — the dispatcher patches it when
        // the server has Tasks enabled, since a pure conversion can't know.
        let input_schema =
            serde_json::from_value(t.input_schema).unwrap_or(legacy::ToolInputSchema {
                properties: BTreeMap::new(),
                required: Vec::new(),
                schema: None,
                type_: "object".to_string(),
            });
        legacy::Tool {
            annotations: None,
            description: t.description,
            execution: None,
            icons: Vec::new(),
            input_schema,
            meta: Map::new(),
            name: t.name,
            output_schema: None,
            title: t.title,
        }
    }
}

impl From<ListToolsResult> for legacy::ListToolsResult {
    fn from(r: ListToolsResult) -> Self {
        legacy::ListToolsResult {
            meta: Map::new(),
            next_cursor: r.next_cursor,
            tools: r.tools.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<CallToolResult> for legacy::CallToolResult {
    fn from(r: CallToolResult) -> Self {
        // The legacy wire requires `structuredContent` to be a JSON object;
        // a non-object neutral value is dropped (documented above).
        let structured_content = match r.structured_content {
            Some(Value::Object(map)) => map,
            _ => Map::new(),
        };
        legacy::CallToolResult {
            content: r.content.into_iter().map(Into::into).collect(),
            is_error: Some(r.is_error),
            meta: Map::new(),
            structured_content,
        }
    }
}

// resources

impl From<Resource> for legacy::Resource {
    fn from(r: Resource) -> Self {
        legacy::Resource {
            annotations: None,
            description: r.description,
            icons: Vec::new(),
            meta: Map::new(),
            mime_type: r.mime_type,
            name: r.name,
            size: r.size.map(|s| i64::try_from(s).unwrap_or(i64::MAX)),
            title: r.title,
            uri: r.uri,
        }
    }
}

impl From<ResourceContents> for legacy::ReadResourceResultContentsItem {
    fn from(c: ResourceContents) -> Self {
        match c {
            ResourceContents::Text {
                uri,
                mime_type,
                text,
            } => legacy::ReadResourceResultContentsItem::TextResourceContents(
                legacy::TextResourceContents {
                    meta: Map::new(),
                    mime_type,
                    text,
                    uri,
                },
            ),
            ResourceContents::Blob {
                uri,
                mime_type,
                blob,
            } => legacy::ReadResourceResultContentsItem::BlobResourceContents(
                legacy::BlobResourceContents {
                    blob,
                    meta: Map::new(),
                    mime_type,
                    uri,
                },
            ),
        }
    }
}

impl From<ListResourcesResult> for legacy::ListResourcesResult {
    fn from(r: ListResourcesResult) -> Self {
        legacy::ListResourcesResult {
            meta: Map::new(),
            next_cursor: r.next_cursor,
            resources: r.resources.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<ReadResourceResult> for legacy::ReadResourceResult {
    fn from(r: ReadResourceResult) -> Self {
        legacy::ReadResourceResult {
            contents: r.contents.into_iter().map(Into::into).collect(),
            meta: Map::new(),
        }
    }
}

impl From<ResourceTemplate> for legacy::ResourceTemplate {
    fn from(t: ResourceTemplate) -> Self {
        legacy::ResourceTemplate {
            annotations: None,
            description: t.description,
            icons: Vec::new(),
            meta: Map::new(),
            mime_type: t.mime_type,
            name: t.name,
            title: t.title,
            uri_template: t.uri_template,
        }
    }
}

impl From<ListResourceTemplatesResult> for legacy::ListResourceTemplatesResult {
    fn from(r: ListResourceTemplatesResult) -> Self {
        legacy::ListResourceTemplatesResult {
            meta: Map::new(),
            next_cursor: r.next_cursor,
            resource_templates: r.resource_templates.into_iter().map(Into::into).collect(),
        }
    }
}

// prompts

impl From<Role> for legacy::Role {
    fn from(r: Role) -> Self {
        match r {
            Role::User => legacy::Role::User,
            Role::Assistant => legacy::Role::Assistant,
        }
    }
}

impl From<PromptArgument> for legacy::PromptArgument {
    fn from(a: PromptArgument) -> Self {
        legacy::PromptArgument {
            description: a.description,
            name: a.name,
            required: Some(a.required),
            title: a.title,
        }
    }
}

impl From<Prompt> for legacy::Prompt {
    fn from(p: Prompt) -> Self {
        legacy::Prompt {
            arguments: p.arguments.into_iter().map(Into::into).collect(),
            description: p.description,
            icons: Vec::new(),
            meta: Map::new(),
            name: p.name,
            title: p.title,
        }
    }
}

impl From<PromptMessage> for legacy::PromptMessage {
    fn from(m: PromptMessage) -> Self {
        legacy::PromptMessage {
            content: m.content.into(),
            role: m.role.into(),
        }
    }
}

impl From<ListPromptsResult> for legacy::ListPromptsResult {
    fn from(r: ListPromptsResult) -> Self {
        legacy::ListPromptsResult {
            meta: Map::new(),
            next_cursor: r.next_cursor,
            prompts: r.prompts.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<GetPromptResult> for legacy::GetPromptResult {
    fn from(r: GetPromptResult) -> Self {
        legacy::GetPromptResult {
            description: r.description,
            messages: r.messages.into_iter().map(Into::into).collect(),
            meta: Map::new(),
        }
    }
}

// completions

impl From<CompleteResult> for legacy::CompleteResult {
    fn from(r: CompleteResult) -> Self {
        legacy::CompleteResult {
            completion: legacy::CompleteResultCompletion {
                has_more: r.has_more,
                total: r.total.map(i64::from),
                values: r.values,
            },
            meta: Map::new(),
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

    #[test]
    fn read_resource_text_widens_to_wire_union() {
        let wire: draft::ReadResourceResult = ReadResourceResult::text("file://a", "hi").into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["resultType"], "complete");
        assert_eq!(v["cacheScope"], "private");
        assert_eq!(v["contents"][0]["uri"], "file://a");
        assert_eq!(v["contents"][0]["text"], "hi");
    }

    #[test]
    fn list_resources_and_templates_fill_required_fields() {
        let res: draft::ListResourcesResult = ListResourcesResult::new(alloc::vec![
            Resource::new("file://a", "a").with_mime_type("text/plain"),
        ])
        .into();
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["resultType"], "complete");
        assert_eq!(v["resources"][0]["mimeType"], "text/plain");

        let templates: draft::ListResourceTemplatesResult = ListResourceTemplatesResult::new(
            alloc::vec![ResourceTemplate::new("file://{path}", "files",)],
        )
        .into();
        let v = serde_json::to_value(&templates).unwrap();
        assert_eq!(v["resourceTemplates"][0]["uriTemplate"], "file://{path}");
        assert_eq!(v["cacheScope"], "private");
    }

    #[test]
    fn prompt_get_widens_with_roles() {
        let wire: draft::GetPromptResult = GetPromptResult::new(alloc::vec![
            PromptMessage::user_text("hello"),
            PromptMessage::assistant_text("hi there"),
        ])
        .with_description("greeting")
        .into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["resultType"], "complete");
        assert_eq!(v["description"], "greeting");
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"]["text"], "hello");
        assert_eq!(v["messages"][1]["role"], "assistant");
    }

    #[test]
    fn list_prompts_carries_arguments() {
        let wire: draft::ListPromptsResult = ListPromptsResult::new(alloc::vec![
            Prompt::new("summarize")
                .with_description("Summarize text")
                .with_argument(PromptArgument::new("text").required(true)),
        ])
        .into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["prompts"][0]["name"], "summarize");
        assert_eq!(v["prompts"][0]["arguments"][0]["name"], "text");
        assert_eq!(v["prompts"][0]["arguments"][0]["required"], true);
    }

    #[test]
    fn complete_result_nests_completion() {
        let wire: draft::CompleteResult = CompleteResult::new(alloc::vec!["foo".to_string()])
            .with_total(1)
            .with_has_more(false)
            .into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["resultType"], "complete");
        assert_eq!(v["completion"]["values"][0], "foo");
        assert_eq!(v["completion"]["total"], 1);
        assert_eq!(v["completion"]["hasMore"], false);
    }

    // ---- legacy (2025-11-25) conversions ----------------------------------

    #[test]
    fn legacy_tool_widens_without_draft_envelope() {
        let wire: legacy::ListToolsResult = ListToolsResult::new(alloc::vec![
            Tool::new(
                "echo",
                json!({"type": "object", "properties": {"msg": {"type": "string"}}, "required": ["msg"]}),
            )
            .with_description("Echoes input"),
        ])
        .into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["tools"][0]["name"], "echo");
        assert_eq!(v["tools"][0]["inputSchema"]["type"], "object");
        assert_eq!(
            v["tools"][0]["inputSchema"]["properties"]["msg"]["type"],
            "string"
        );
        assert_eq!(v["tools"][0]["inputSchema"]["required"][0], "msg");
        // The draft caching envelope must not leak onto the legacy wire.
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("resultType"));
        assert!(!obj.contains_key("cacheScope"));
        assert!(!obj.contains_key("ttlMs"));
    }

    #[test]
    fn legacy_call_result_keeps_object_structured_content_drops_non_object() {
        let mut ok = CallToolResult::text("done");
        ok.structured_content = Some(json!({"answer": 42}));
        let wire: legacy::CallToolResult = ok.into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["isError"], false);
        assert_eq!(v["content"][0]["text"], "done");
        assert_eq!(v["structuredContent"]["answer"], 42);

        let mut bad = CallToolResult::text("done");
        bad.structured_content = Some(json!(7)); // not an object: unrepresentable
        let wire: legacy::CallToolResult = bad.into();
        let v = serde_json::to_value(&wire).unwrap();
        assert!(v.as_object().unwrap().get("structuredContent").is_none());
    }

    #[test]
    fn legacy_resources_round_trip() {
        let wire: legacy::ListResourcesResult = ListResourcesResult::new(alloc::vec![
            Resource::new("file://a", "a").with_mime_type("text/plain"),
        ])
        .into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["resources"][0]["uri"], "file://a");
        assert_eq!(v["resources"][0]["mimeType"], "text/plain");

        let wire: legacy::ReadResourceResult = ReadResourceResult::text("file://a", "hi").into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["contents"][0]["text"], "hi");

        let wire: legacy::ListResourceTemplatesResult = ListResourceTemplatesResult::new(
            alloc::vec![ResourceTemplate::new("file://{path}", "files")],
        )
        .into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["resourceTemplates"][0]["uriTemplate"], "file://{path}");
    }

    #[test]
    fn legacy_prompts_and_completion_round_trip() {
        let wire: legacy::ListPromptsResult = ListPromptsResult::new(alloc::vec![
            Prompt::new("summarize").with_argument(PromptArgument::new("text").required(true)),
        ])
        .into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["prompts"][0]["arguments"][0]["required"], true);

        let wire: legacy::GetPromptResult =
            GetPromptResult::new(alloc::vec![PromptMessage::user_text("hello")]).into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"]["type"], "text");

        let wire: legacy::CompleteResult = CompleteResult::new(alloc::vec!["x".to_string()])
            .with_total(5)
            .into();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["completion"]["total"], 5);
        assert_eq!(v["completion"]["values"][0], "x");
    }
}
