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
//! `turbomcp-server` are expressed in these types, wiring the second wire
//! version (Phase 5) adds conversions here without changing any handler.
//!
//! Conversions are intentionally one-directional (neutral → wire) and total
//! (`From`, never failing): a handler can always be serialized to the wire.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::{Map, Value};

use crate::draft::types as draft;
use crate::v2025_11_25::types as legacy;

/// Canonical draft `resultType` wire strings.
///
/// The draft schema made `ResultType` an open string (SEP-2322: extensible
/// result types); these are the two values the spec defines. Clients MUST
/// treat an absent field as `"complete"`.
pub mod result_type {
    /// The request completed; the result carries the final content.
    pub const COMPLETE: &str = "complete";
    /// The request needs more input; the result is an `InputRequiredResult`.
    pub const INPUT_REQUIRED: &str = "input_required";
}

/// A neutral content block. The enum is `#[non_exhaustive]` so further block
/// kinds (embedded resources, resource links) slot in without breaking callers.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Content {
    /// Plain UTF-8 text.
    Text(String),
    /// Base64-encoded image data with its MIME type (e.g. `image/png`).
    Image {
        /// Base64-encoded image bytes.
        data: String,
        /// The image MIME type.
        mime_type: String,
    },
    /// Base64-encoded audio data with its MIME type (e.g. `audio/wav`).
    Audio {
        /// Base64-encoded audio bytes.
        data: String,
        /// The audio MIME type.
        mime_type: String,
    },
    /// An embedded resource: the resource's contents inline (text or a base64
    /// blob), carried in a message rather than referenced by URI.
    Resource(ResourceContents),
    /// A link to a resource by URI + descriptor (name/title/MIME/size), without
    /// its contents — the client can `resources/read` it.
    ResourceLink(Resource),
}

impl Content {
    /// A text content block.
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    /// An image content block from base64 data and a MIME type.
    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::Image {
            data: data.into(),
            mime_type: mime_type.into(),
        }
    }

    /// An audio content block from base64 data and a MIME type.
    pub fn audio(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::Audio {
            data: data.into(),
            mime_type: mime_type.into(),
        }
    }

    /// An embedded-resource content block carrying the resource contents inline.
    pub fn resource(contents: ResourceContents) -> Self {
        Self::Resource(contents)
    }

    /// A resource-link content block referencing a resource by URI.
    pub fn resource_link(resource: Resource) -> Self {
        Self::ResourceLink(resource)
    }
}

/// A tool descriptor.
/// Whether a tool supports being run as an asynchronous task (`2025-11-25` core
/// Tasks). Mirrors the wire `execution.taskSupport`; the draft models Tasks as a
/// server-directed extension instead, so this rides only the legacy wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskSupport {
    /// The tool must not be run as a task.
    Forbidden,
    /// The tool may be run as a task at the client's request.
    Optional,
    /// The tool is always run as a task.
    Required,
}

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
    /// Optional JSON Schema object describing the tool's structured result
    /// (`structuredContent`). Generated from a `Json<T>` return type.
    pub output_schema: Option<Value>,
    /// Per-tool `2025-11-25` task support (`#[tool(task)]`). `None` leaves it to
    /// the server's global Tasks policy; the draft wire ignores it.
    pub task_support: Option<TaskSupport>,
}

impl Tool {
    /// A tool with the given name and argument schema; no title/description.
    pub fn new(name: impl Into<String>, input_schema: Value) -> Self {
        Self {
            name: name.into(),
            title: None,
            description: None,
            input_schema,
            output_schema: None,
            task_support: None,
        }
    }

    /// Set the description (builder style).
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the output schema (builder style) — the JSON Schema for the tool's
    /// `structuredContent`.
    #[must_use]
    pub fn with_output_schema(mut self, output_schema: Value) -> Self {
        self.output_schema = Some(output_schema);
        self
    }

    /// Set the per-tool task support (builder style) — `#[tool(task)]`.
    #[must_use]
    pub fn with_task_support(mut self, task_support: TaskSupport) -> Self {
        self.task_support = Some(task_support);
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
    /// A successful result carrying the given content blocks (text, image, audio).
    pub fn new(content: Vec<Content>) -> Self {
        Self {
            content,
            is_error: false,
            structured_content: None,
        }
    }

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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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

// ---- elicitation (client interaction) ------------------------------------------

/// What a handler asks the user for via `ctx.client.elicit(…)` (form mode).
///
/// On the draft this is packaged into an `InputRequiredResult` (MRTR,
/// SEP-2322); on `2025-11-25` it goes out as an inline `elicitation/create`
/// request. For URL-mode (out-of-band) elicitation see [`ElicitUrlParams`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ElicitParams {
    /// The message presented to the user describing what is being requested.
    pub message: String,
    /// The requested form schema — the spec's restricted JSON Schema subset
    /// (top-level primitive properties only, no nesting).
    pub requested_schema: Value,
}

impl ElicitParams {
    /// An elicitation showing `message` and requesting `requested_schema`.
    pub fn new(message: impl Into<String>, requested_schema: Value) -> Self {
        Self {
            message: message.into(),
            requested_schema,
        }
    }
}

/// A URL-mode elicitation (draft `mode: "url"`): the client shows `message` and
/// directs the user to `url` (e.g. an OAuth consent page); the response carries
/// an [`ElicitAction`] but no form content. `elicitation_id` is a server-unique
/// opaque identifier the client echoes / uses for any out-of-band completion.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ElicitUrlParams {
    /// The message explaining why the interaction is needed.
    pub message: String,
    /// A server-unique opaque id for this elicitation.
    pub elicitation_id: String,
    /// The URL the user should navigate to.
    pub url: String,
}

impl ElicitUrlParams {
    /// A URL-mode elicitation showing `message` and directing the user to `url`.
    pub fn new(
        message: impl Into<String>,
        elicitation_id: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            elicitation_id: elicitation_id.into(),
            url: url.into(),
        }
    }
}

/// The user's action in response to an elicitation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElicitAction {
    /// The user submitted the form / confirmed the action.
    Accept,
    /// The user explicitly declined.
    Decline,
    /// The user dismissed without an explicit choice.
    Cancel,
}

/// What the client answered to an elicitation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ElicitOutcome {
    /// The user's action.
    pub action: ElicitAction,
    /// Submitted form values (present only on `Accept` in form mode).
    pub content: Map<String, Value>,
}

impl ElicitOutcome {
    /// An outcome with the given action and submitted content.
    #[must_use]
    pub fn new(action: ElicitAction, content: Map<String, Value>) -> Self {
        Self { action, content }
    }

    /// Whether the user accepted.
    #[must_use]
    pub fn accepted(&self) -> bool {
        self.action == ElicitAction::Accept
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

// ---- neutral → 2026-07-28 wire conversions --------------------------------

impl From<Content> for draft::ContentBlock {
    fn from(c: Content) -> Self {
        match c {
            Content::Text(text) => draft::ContentBlock::TextContent(draft::TextContent {
                annotations: None,
                meta: None,
                text,
                type_: "text".to_string(),
            }),
            Content::Image { data, mime_type } => {
                draft::ContentBlock::ImageContent(draft::ImageContent {
                    annotations: None,
                    data,
                    meta: None,
                    mime_type,
                    type_: "image".to_string(),
                })
            }
            Content::Audio { data, mime_type } => {
                draft::ContentBlock::AudioContent(draft::AudioContent {
                    annotations: None,
                    data,
                    meta: None,
                    mime_type,
                    type_: "audio".to_string(),
                })
            }
            Content::Resource(contents) => {
                draft::ContentBlock::EmbeddedResource(draft::EmbeddedResource {
                    annotations: None,
                    meta: None,
                    resource: contents.into(),
                    type_: "resource".to_string(),
                })
            }
            Content::ResourceLink(r) => draft::ContentBlock::ResourceLink(draft::ResourceLink {
                annotations: None,
                description: r.description,
                icons: alloc::vec::Vec::new(),
                meta: None,
                mime_type: r.mime_type,
                name: r.name,
                size: r.size.and_then(|s| i64::try_from(s).ok()),
                title: r.title,
                type_: "resource_link".to_string(),
                uri: r.uri,
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
            // The draft `ToolOutputSchema` is a permissive bag ($schema + a
            // flattened `extra`), so any JSON Schema object deserializes into it.
            output_schema: t.output_schema.and_then(|v| serde_json::from_value(v).ok()),
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
            result_type: result_type::COMPLETE.to_string(),
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
            result_type: result_type::COMPLETE.to_string(),
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

impl From<ResourceContents> for draft::EmbeddedResourceResource {
    fn from(c: ResourceContents) -> Self {
        match c {
            ResourceContents::Text {
                uri,
                mime_type,
                text,
            } => {
                draft::EmbeddedResourceResource::TextResourceContents(draft::TextResourceContents {
                    meta: None,
                    mime_type,
                    text,
                    uri,
                })
            }
            ResourceContents::Blob {
                uri,
                mime_type,
                blob,
            } => {
                draft::EmbeddedResourceResource::BlobResourceContents(draft::BlobResourceContents {
                    blob,
                    meta: None,
                    mime_type,
                    uri,
                })
            }
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
            result_type: result_type::COMPLETE.to_string(),
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
            result_type: result_type::COMPLETE.to_string(),
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
            result_type: result_type::COMPLETE.to_string(),
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
            result_type: result_type::COMPLETE.to_string(),
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
            result_type: result_type::COMPLETE.to_string(),
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
            result_type: result_type::COMPLETE.to_string(),
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
            Content::Image { data, mime_type } => {
                legacy::ContentBlock::ImageContent(legacy::ImageContent {
                    annotations: None,
                    data,
                    meta: Map::new(),
                    mime_type,
                    type_: "image".to_string(),
                })
            }
            Content::Audio { data, mime_type } => {
                legacy::ContentBlock::AudioContent(legacy::AudioContent {
                    annotations: None,
                    data,
                    meta: Map::new(),
                    mime_type,
                    type_: "audio".to_string(),
                })
            }
            Content::Resource(contents) => {
                legacy::ContentBlock::EmbeddedResource(legacy::EmbeddedResource {
                    annotations: None,
                    meta: Map::new(),
                    resource: contents.into(),
                    type_: "resource".to_string(),
                })
            }
            Content::ResourceLink(r) => legacy::ContentBlock::ResourceLink(legacy::ResourceLink {
                annotations: None,
                description: r.description,
                icons: alloc::vec::Vec::new(),
                meta: Map::new(),
                mime_type: r.mime_type,
                name: r.name,
                size: r.size.and_then(|s| i64::try_from(s).ok()),
                title: r.title,
                type_: "resource_link".to_string(),
                uri: r.uri,
            }),
        }
    }
}

impl From<TaskSupport> for legacy::ToolExecutionTaskSupport {
    fn from(ts: TaskSupport) -> Self {
        match ts {
            TaskSupport::Forbidden => legacy::ToolExecutionTaskSupport::Forbidden,
            TaskSupport::Optional => legacy::ToolExecutionTaskSupport::Optional,
            TaskSupport::Required => legacy::ToolExecutionTaskSupport::Required,
        }
    }
}

impl From<legacy::ToolExecutionTaskSupport> for TaskSupport {
    fn from(ts: legacy::ToolExecutionTaskSupport) -> Self {
        match ts {
            legacy::ToolExecutionTaskSupport::Forbidden => TaskSupport::Forbidden,
            legacy::ToolExecutionTaskSupport::Optional => TaskSupport::Optional,
            legacy::ToolExecutionTaskSupport::Required => TaskSupport::Required,
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
            // A declared `#[tool(task)]` sets per-tool task support; otherwise
            // left unset for the dispatcher to patch under a global Tasks policy.
            execution: t.task_support.map(|ts| legacy::ToolExecution {
                task_support: Some(ts.into()),
            }),
            icons: Vec::new(),
            input_schema,
            meta: Map::new(),
            name: t.name,
            // The legacy `ToolOutputSchema` is a closed object schema (requires
            // `type`); a schemars-generated struct schema deserializes cleanly,
            // and anything that doesn't is dropped rather than failing.
            output_schema: t.output_schema.and_then(|v| serde_json::from_value(v).ok()),
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

impl From<ResourceContents> for legacy::EmbeddedResourceResource {
    fn from(c: ResourceContents) -> Self {
        match c {
            ResourceContents::Text {
                uri,
                mime_type,
                text,
            } => legacy::EmbeddedResourceResource::TextResourceContents(
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
            } => legacy::EmbeddedResourceResource::BlobResourceContents(
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

// ---- wire → neutral conversions (the client's inbound path) -------------------
//
// The inverse of every conversion above: a client deserializes a server's
// result into the negotiated version's wire type (codegenned, spec-exact) and
// narrows it to the neutral common subset. Version-specific envelope fields
// (`resultType` / `cacheScope` / `ttlMs` / `_meta` / annotations / icons) are
// dropped — they aren't part of the cross-version surface a client consumes.
//
// **Content is fully modeled.** Neutral `Content` covers every wire content
// block kind — text, image, audio, embedded resource, and resource link — so
// inbound conversion is total (no lossy JSON-text fallback).

impl From<draft::ContentBlock> for Content {
    fn from(c: draft::ContentBlock) -> Self {
        match c {
            draft::ContentBlock::TextContent(t) => Content::Text(t.text),
            draft::ContentBlock::ImageContent(i) => Content::Image {
                data: i.data,
                mime_type: i.mime_type,
            },
            draft::ContentBlock::AudioContent(a) => Content::Audio {
                data: a.data,
                mime_type: a.mime_type,
            },
            draft::ContentBlock::EmbeddedResource(e) => Content::Resource(e.resource.into()),
            draft::ContentBlock::ResourceLink(l) => Content::ResourceLink(l.into()),
        }
    }
}

impl From<draft::Tool> for Tool {
    fn from(t: draft::Tool) -> Self {
        Tool {
            name: t.name,
            title: t.title,
            description: t.description,
            input_schema: serde_json::to_value(&t.input_schema)
                .unwrap_or_else(|_| Value::Object(Map::new())),
            output_schema: t.output_schema.and_then(|s| serde_json::to_value(s).ok()),
            // The draft models Tasks as a server-directed extension, not a
            // per-tool wire field.
            task_support: None,
        }
    }
}

impl From<draft::ListToolsResult> for ListToolsResult {
    fn from(r: draft::ListToolsResult) -> Self {
        ListToolsResult {
            tools: r.tools.into_iter().map(Into::into).collect(),
            next_cursor: r.next_cursor,
        }
    }
}

impl From<draft::CallToolResult> for CallToolResult {
    fn from(r: draft::CallToolResult) -> Self {
        CallToolResult {
            content: r.content.into_iter().map(Into::into).collect(),
            is_error: r.is_error.unwrap_or(false),
            structured_content: r.structured_content,
        }
    }
}

impl From<draft::Resource> for Resource {
    fn from(r: draft::Resource) -> Self {
        Resource {
            uri: r.uri,
            name: r.name,
            title: r.title,
            description: r.description,
            mime_type: r.mime_type,
            size: r.size.map(|s| u64::try_from(s).unwrap_or(0)),
        }
    }
}

impl From<draft::ResourceLink> for Resource {
    fn from(l: draft::ResourceLink) -> Self {
        Resource {
            uri: l.uri,
            name: l.name,
            title: l.title,
            description: l.description,
            mime_type: l.mime_type,
            size: l.size.map(|s| u64::try_from(s).unwrap_or(0)),
        }
    }
}

impl From<draft::EmbeddedResourceResource> for ResourceContents {
    fn from(r: draft::EmbeddedResourceResource) -> Self {
        match r {
            draft::EmbeddedResourceResource::TextResourceContents(t) => ResourceContents::Text {
                uri: t.uri,
                mime_type: t.mime_type,
                text: t.text,
            },
            draft::EmbeddedResourceResource::BlobResourceContents(b) => ResourceContents::Blob {
                uri: b.uri,
                mime_type: b.mime_type,
                blob: b.blob,
            },
        }
    }
}

impl From<draft::ReadResourceResultContentsItem> for ResourceContents {
    fn from(c: draft::ReadResourceResultContentsItem) -> Self {
        match c {
            draft::ReadResourceResultContentsItem::TextResourceContents(t) => {
                ResourceContents::Text {
                    uri: t.uri,
                    mime_type: t.mime_type,
                    text: t.text,
                }
            }
            draft::ReadResourceResultContentsItem::BlobResourceContents(b) => {
                ResourceContents::Blob {
                    uri: b.uri,
                    mime_type: b.mime_type,
                    blob: b.blob,
                }
            }
        }
    }
}

impl From<draft::ListResourcesResult> for ListResourcesResult {
    fn from(r: draft::ListResourcesResult) -> Self {
        ListResourcesResult {
            resources: r.resources.into_iter().map(Into::into).collect(),
            next_cursor: r.next_cursor,
        }
    }
}

impl From<draft::ReadResourceResult> for ReadResourceResult {
    fn from(r: draft::ReadResourceResult) -> Self {
        ReadResourceResult {
            contents: r.contents.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<draft::ResourceTemplate> for ResourceTemplate {
    fn from(t: draft::ResourceTemplate) -> Self {
        ResourceTemplate {
            uri_template: t.uri_template,
            name: t.name,
            title: t.title,
            description: t.description,
            mime_type: t.mime_type,
        }
    }
}

impl From<draft::ListResourceTemplatesResult> for ListResourceTemplatesResult {
    fn from(r: draft::ListResourceTemplatesResult) -> Self {
        ListResourceTemplatesResult {
            resource_templates: r.resource_templates.into_iter().map(Into::into).collect(),
            next_cursor: r.next_cursor,
        }
    }
}

impl From<draft::Role> for Role {
    fn from(r: draft::Role) -> Self {
        match r {
            draft::Role::User => Role::User,
            draft::Role::Assistant => Role::Assistant,
        }
    }
}

impl From<draft::PromptArgument> for PromptArgument {
    fn from(a: draft::PromptArgument) -> Self {
        PromptArgument {
            name: a.name,
            title: a.title,
            description: a.description,
            required: a.required.unwrap_or(false),
        }
    }
}

impl From<draft::Prompt> for Prompt {
    fn from(p: draft::Prompt) -> Self {
        Prompt {
            name: p.name,
            title: p.title,
            description: p.description,
            arguments: p.arguments.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<draft::PromptMessage> for PromptMessage {
    fn from(m: draft::PromptMessage) -> Self {
        PromptMessage {
            role: m.role.into(),
            content: m.content.into(),
        }
    }
}

impl From<draft::ListPromptsResult> for ListPromptsResult {
    fn from(r: draft::ListPromptsResult) -> Self {
        ListPromptsResult {
            prompts: r.prompts.into_iter().map(Into::into).collect(),
            next_cursor: r.next_cursor,
        }
    }
}

impl From<draft::GetPromptResult> for GetPromptResult {
    fn from(r: draft::GetPromptResult) -> Self {
        GetPromptResult {
            description: r.description,
            messages: r.messages.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<draft::CompleteResult> for CompleteResult {
    fn from(r: draft::CompleteResult) -> Self {
        CompleteResult {
            values: r.completion.values,
            total: r
                .completion
                .total
                .map(|t| u32::try_from(t).unwrap_or(u32::MAX)),
            has_more: r.completion.has_more,
        }
    }
}

// ---- 2025-11-25 wire → neutral ------------------------------------------------

impl From<legacy::ContentBlock> for Content {
    fn from(c: legacy::ContentBlock) -> Self {
        match c {
            legacy::ContentBlock::TextContent(t) => Content::Text(t.text),
            legacy::ContentBlock::ImageContent(i) => Content::Image {
                data: i.data,
                mime_type: i.mime_type,
            },
            legacy::ContentBlock::AudioContent(a) => Content::Audio {
                data: a.data,
                mime_type: a.mime_type,
            },
            legacy::ContentBlock::EmbeddedResource(e) => Content::Resource(e.resource.into()),
            legacy::ContentBlock::ResourceLink(l) => Content::ResourceLink(l.into()),
        }
    }
}

impl From<legacy::Tool> for Tool {
    fn from(t: legacy::Tool) -> Self {
        Tool {
            name: t.name,
            title: t.title,
            description: t.description,
            input_schema: serde_json::to_value(&t.input_schema)
                .unwrap_or_else(|_| Value::Object(Map::new())),
            output_schema: t.output_schema.and_then(|s| serde_json::to_value(s).ok()),
            task_support: t.execution.and_then(|e| e.task_support).map(Into::into),
        }
    }
}

impl From<legacy::ListToolsResult> for ListToolsResult {
    fn from(r: legacy::ListToolsResult) -> Self {
        ListToolsResult {
            tools: r.tools.into_iter().map(Into::into).collect(),
            next_cursor: r.next_cursor,
        }
    }
}

impl From<legacy::CallToolResult> for CallToolResult {
    fn from(r: legacy::CallToolResult) -> Self {
        CallToolResult {
            content: r.content.into_iter().map(Into::into).collect(),
            is_error: r.is_error.unwrap_or(false),
            structured_content: if r.structured_content.is_empty() {
                None
            } else {
                Some(Value::Object(r.structured_content))
            },
        }
    }
}

impl From<legacy::Resource> for Resource {
    fn from(r: legacy::Resource) -> Self {
        Resource {
            uri: r.uri,
            name: r.name,
            title: r.title,
            description: r.description,
            mime_type: r.mime_type,
            size: r.size.map(|s| u64::try_from(s).unwrap_or(0)),
        }
    }
}

impl From<legacy::ResourceLink> for Resource {
    fn from(l: legacy::ResourceLink) -> Self {
        Resource {
            uri: l.uri,
            name: l.name,
            title: l.title,
            description: l.description,
            mime_type: l.mime_type,
            size: l.size.map(|s| u64::try_from(s).unwrap_or(0)),
        }
    }
}

impl From<legacy::EmbeddedResourceResource> for ResourceContents {
    fn from(r: legacy::EmbeddedResourceResource) -> Self {
        match r {
            legacy::EmbeddedResourceResource::TextResourceContents(t) => ResourceContents::Text {
                uri: t.uri,
                mime_type: t.mime_type,
                text: t.text,
            },
            legacy::EmbeddedResourceResource::BlobResourceContents(b) => ResourceContents::Blob {
                uri: b.uri,
                mime_type: b.mime_type,
                blob: b.blob,
            },
        }
    }
}

impl From<legacy::ReadResourceResultContentsItem> for ResourceContents {
    fn from(c: legacy::ReadResourceResultContentsItem) -> Self {
        match c {
            legacy::ReadResourceResultContentsItem::TextResourceContents(t) => {
                ResourceContents::Text {
                    uri: t.uri,
                    mime_type: t.mime_type,
                    text: t.text,
                }
            }
            legacy::ReadResourceResultContentsItem::BlobResourceContents(b) => {
                ResourceContents::Blob {
                    uri: b.uri,
                    mime_type: b.mime_type,
                    blob: b.blob,
                }
            }
        }
    }
}

impl From<legacy::ListResourcesResult> for ListResourcesResult {
    fn from(r: legacy::ListResourcesResult) -> Self {
        ListResourcesResult {
            resources: r.resources.into_iter().map(Into::into).collect(),
            next_cursor: r.next_cursor,
        }
    }
}

impl From<legacy::ReadResourceResult> for ReadResourceResult {
    fn from(r: legacy::ReadResourceResult) -> Self {
        ReadResourceResult {
            contents: r.contents.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<legacy::ResourceTemplate> for ResourceTemplate {
    fn from(t: legacy::ResourceTemplate) -> Self {
        ResourceTemplate {
            uri_template: t.uri_template,
            name: t.name,
            title: t.title,
            description: t.description,
            mime_type: t.mime_type,
        }
    }
}

impl From<legacy::ListResourceTemplatesResult> for ListResourceTemplatesResult {
    fn from(r: legacy::ListResourceTemplatesResult) -> Self {
        ListResourceTemplatesResult {
            resource_templates: r.resource_templates.into_iter().map(Into::into).collect(),
            next_cursor: r.next_cursor,
        }
    }
}

impl From<legacy::Role> for Role {
    fn from(r: legacy::Role) -> Self {
        match r {
            legacy::Role::User => Role::User,
            legacy::Role::Assistant => Role::Assistant,
        }
    }
}

impl From<legacy::PromptArgument> for PromptArgument {
    fn from(a: legacy::PromptArgument) -> Self {
        PromptArgument {
            name: a.name,
            title: a.title,
            description: a.description,
            required: a.required.unwrap_or(false),
        }
    }
}

impl From<legacy::Prompt> for Prompt {
    fn from(p: legacy::Prompt) -> Self {
        Prompt {
            name: p.name,
            title: p.title,
            description: p.description,
            arguments: p.arguments.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<legacy::PromptMessage> for PromptMessage {
    fn from(m: legacy::PromptMessage) -> Self {
        PromptMessage {
            role: m.role.into(),
            content: m.content.into(),
        }
    }
}

impl From<legacy::ListPromptsResult> for ListPromptsResult {
    fn from(r: legacy::ListPromptsResult) -> Self {
        ListPromptsResult {
            prompts: r.prompts.into_iter().map(Into::into).collect(),
            next_cursor: r.next_cursor,
        }
    }
}

impl From<legacy::GetPromptResult> for GetPromptResult {
    fn from(r: legacy::GetPromptResult) -> Self {
        GetPromptResult {
            description: r.description,
            messages: r.messages.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<legacy::CompleteResult> for CompleteResult {
    fn from(r: legacy::CompleteResult) -> Self {
        CompleteResult {
            values: r.completion.values,
            total: r
                .completion
                .total
                .map(|t| u32::try_from(t).unwrap_or(u32::MAX)),
            has_more: r.completion.has_more,
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

    // ---- wire → neutral (the client's inbound path) -----------------------
    //
    // Round-trip the common subset: neutral → version wire → neutral must
    // preserve every field neutral models. The version envelope (resultType,
    // cacheScope, …) is dropped on the way back, which is the point.

    #[test]
    fn draft_tools_round_trip_through_neutral() {
        let original = ListToolsResult::new(alloc::vec![
            Tool::new("echo", json!({"type": "object"}))
                .with_title("Echo")
                .with_description("Echoes input"),
        ]);
        let wire: draft::ListToolsResult = original.into();
        let back: ListToolsResult = wire.into();
        assert_eq!(back.tools.len(), 1);
        assert_eq!(back.tools[0].name, "echo");
        assert_eq!(back.tools[0].title.as_deref(), Some("Echo"));
        assert_eq!(back.tools[0].description.as_deref(), Some("Echoes input"));
        assert_eq!(back.tools[0].input_schema["type"], "object");
    }

    #[test]
    fn legacy_tools_round_trip_through_neutral() {
        let original =
            ListToolsResult::new(alloc::vec![Tool::new("add", json!({"type": "object"}))]);
        let wire: legacy::ListToolsResult = original.into();
        let back: ListToolsResult = wire.into();
        assert_eq!(back.tools[0].name, "add");
    }

    #[test]
    fn draft_call_result_round_trips_content_and_is_error() {
        let original = CallToolResult::error("boom");
        let wire: draft::CallToolResult = original.into();
        let back: CallToolResult = wire.into();
        assert!(back.is_error);
        assert_eq!(back.content.len(), 1);
        assert!(matches!(&back.content[0], Content::Text(t) if t == "boom"));
    }

    #[test]
    fn legacy_call_result_object_structured_content_round_trips() {
        let mut original = CallToolResult::text("ok");
        original.structured_content = Some(json!({"answer": 42}));
        let wire: legacy::CallToolResult = original.into();
        let back: CallToolResult = wire.into();
        assert!(!back.is_error);
        assert_eq!(back.structured_content, Some(json!({"answer": 42})));
    }

    #[test]
    fn read_resource_round_trips_text_and_blob() {
        let original = ReadResourceResult::new(alloc::vec![
            ResourceContents::text("file://a", "hi").with_mime_type("text/plain"),
            ResourceContents::blob("file://b", "Zm9v"),
        ]);
        let wire: draft::ReadResourceResult = original.into();
        let back: ReadResourceResult = wire.into();
        assert_eq!(back.contents.len(), 2);
        assert!(
            matches!(&back.contents[0], ResourceContents::Text { uri, text, mime_type }
                if uri == "file://a" && text == "hi" && mime_type.as_deref() == Some("text/plain"))
        );
        assert!(
            matches!(&back.contents[1], ResourceContents::Blob { uri, blob, .. }
                if uri == "file://b" && blob == "Zm9v")
        );
    }

    #[test]
    fn prompts_and_completion_round_trip() {
        let prompts = ListPromptsResult::new(alloc::vec![
            Prompt::new("summarize")
                .with_description("Summarize text")
                .with_argument(PromptArgument::new("text").required(true)),
        ]);
        let wire: draft::ListPromptsResult = prompts.into();
        let back: ListPromptsResult = wire.into();
        assert_eq!(back.prompts[0].name, "summarize");
        assert!(back.prompts[0].arguments[0].required);

        let get = GetPromptResult::new(alloc::vec![PromptMessage::user_text("hello")])
            .with_description("greeting");
        let wire: legacy::GetPromptResult = get.into();
        let back: GetPromptResult = wire.into();
        assert_eq!(back.description.as_deref(), Some("greeting"));
        assert!(matches!(&back.messages[0].content, Content::Text(t) if t == "hello"));
        assert!(matches!(back.messages[0].role, Role::User));

        let complete = CompleteResult::new(alloc::vec!["foo".to_string()])
            .with_total(1)
            .with_has_more(false);
        let wire: draft::CompleteResult = complete.into();
        let back: CompleteResult = wire.into();
        assert_eq!(back.values, alloc::vec!["foo".to_string()]);
        assert_eq!(back.total, Some(1));
        assert_eq!(back.has_more, Some(false));
    }

    #[test]
    fn image_and_audio_content_round_trip() {
        for content in [
            Content::image("Zm9v", "image/png"),
            Content::audio("YmFy", "audio/wav"),
        ] {
            // draft
            let draft_block: draft::ContentBlock = content.clone().into();
            assert_eq!(Content::from(draft_block), content);
            // legacy
            let legacy_block: legacy::ContentBlock = content.clone().into();
            assert_eq!(Content::from(legacy_block), content);
        }
    }

    #[test]
    fn resource_and_resource_link_content_round_trip() {
        for content in [
            Content::resource(
                ResourceContents::text("file://x", "hi").with_mime_type("text/plain"),
            ),
            Content::resource(
                ResourceContents::blob("file://y", "Zm9v").with_mime_type("image/png"),
            ),
            Content::resource_link(
                Resource::new("file://x", "x")
                    .with_title("X")
                    .with_mime_type("text/plain"),
            ),
        ] {
            // draft
            let draft_block: draft::ContentBlock = content.clone().into();
            assert_eq!(Content::from(draft_block), content);
            // legacy
            let legacy_block: legacy::ContentBlock = content.clone().into();
            assert_eq!(Content::from(legacy_block), content);
        }
    }
}
