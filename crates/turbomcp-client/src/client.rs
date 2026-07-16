//! The typed, version-negotiated MCP client.
//!
//! [`Client`] wraps a raw [`Connection`] with everything a protocol-aware client
//! needs: it runs the `initialize` / `server/discover` handshake, remembers the
//! negotiated [`ProtocolVersion`], stamps the modern `_meta` envelope onto every
//! outbound request, and decodes results from the negotiated version's wire
//! shape into version-stable [`neutral`] types.
//!
//! Build one with [`ClientBuilder`]:
//!
//! ```no_run
//! # async fn f(transport: impl turbomcp_service::Transport) -> turbomcp_client::ClientResult<()> {
//! use turbomcp_client::ClientBuilder;
//! let client = ClientBuilder::new("my-client", "1.0.0").connect(transport).await?;
//! let tools = client.list_tools(None).await?;
//! # Ok(()) }
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use turbomcp_core::meta::keys;
use turbomcp_core::{Implementation, ProtocolVersion};
use turbomcp_protocol::draft::types as draft;
use turbomcp_protocol::methods::{notification, request};
use turbomcp_protocol::neutral;
use turbomcp_protocol::v2025_11_25::types as legacy;
use turbomcp_service::{Transport, mcp_headers};

use crate::connection::Connection;
use crate::error::{ClientError, ClientResult};
use crate::handler::{ClientHandler, dispatch_server_request};

/// Cap on MRTR re-issue rounds — a guard against a server that keeps answering
/// `input_required` forever.
const MAX_MRTR_ROUNDS: usize = 16;

/// Internal `_meta` key carrying the `#[mcp_header]` mirror map — header-name
/// portion → already-encoded header value — to emit as `Mcp-Param-*` headers.
/// Consumed and stripped by the HTTP transport (and sanitized server-side as
/// an `io.turbomcp.internal/*` key on other transports), so it never reaches
/// a handler.
pub(crate) const HEADER_PARAMS_META_KEY: &str = "io.turbomcp.internal/headerParams";

/// Internal `_meta` key carrying the negotiated protocol version for the HTTP
/// transport's `MCP-Protocol-Version` header (required on every POST by both
/// versions' transports specs). Stripped by the HTTP transport; sanitized at
/// every server boundary otherwise.
pub(crate) const NEGOTIATED_VERSION_META_KEY: &str = "io.turbomcp.internal/negotiatedVersion";

/// How a [`Client`] decides which protocol version to speak.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConnectMode {
    /// Probe the modern (`server/discover`) path first; on `-32601`/`-32022`
    /// fall back to the legacy `initialize` handshake. The default.
    #[default]
    Auto,
    /// Force the modern, stateless `2026-07-28` path (`server/discover`).
    Modern,
    /// Force the legacy `2025-11-25` path (`initialize` + `notifications/initialized`).
    Legacy,
}

/// Builds a [`Client`]: identity, advertised capabilities, connect mode, and
/// timeout, then [`connect`](ClientBuilder::connect)s over a transport.
#[derive(Clone)]
pub struct ClientBuilder {
    client_info: Implementation,
    capabilities: Value,
    connect_mode: ConnectMode,
    request_timeout: Duration,
    handler: Option<Arc<dyn ClientHandler>>,
}

impl ClientBuilder {
    /// Start a builder for a client identifying as `name`/`version`.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            client_info: Implementation::new(name, version),
            capabilities: Value::Object(Map::new()),
            connect_mode: ConnectMode::Auto,
            request_timeout: crate::connection::DEFAULT_REQUEST_TIMEOUT,
            handler: None,
        }
    }

    /// Set the [`ClientHandler`] that answers server→client requests
    /// (elicitation, sampling, roots). Required to answer elicitation on either
    /// version — and, on the modern path, also drives the MRTR loop.
    #[must_use]
    pub fn with_handler<H: ClientHandler>(mut self, handler: H) -> Self {
        self.handler = Some(Arc::new(handler));
        self
    }

    /// Set the capabilities this client advertises (e.g. `elicitation`,
    /// `sampling`, `roots`). Sent in the handshake and, on the modern path,
    /// stamped into every request's `_meta`. Defaults to `{}`.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Value) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Choose the connect mode (default [`ConnectMode::Auto`]).
    #[must_use]
    pub fn with_connect_mode(mut self, mode: ConnectMode) -> Self {
        self.connect_mode = mode;
        self
    }

    /// Set the per-request timeout (default 60s).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Spawn the connection over `transport`, run the handshake, and return a
    /// ready [`Client`].
    ///
    /// # Errors
    /// [`ClientError::Protocol`] if no supported version could be negotiated, or
    /// the underlying connection/handshake failure.
    pub async fn connect<T>(self, transport: T) -> ClientResult<Client>
    where
        T: Transport,
    {
        let conn = Connection::connect(transport, self.request_timeout, self.handler.clone());
        self.handshake(conn).await
    }

    /// Drive the handshake per the configured mode, returning a [`Client`].
    async fn handshake(self, conn: Connection) -> ClientResult<Client> {
        let outcome = match self.connect_mode {
            ConnectMode::Modern => self.modern_handshake(&conn).await?,
            ConnectMode::Legacy => self.legacy_handshake(&conn).await?,
            ConnectMode::Auto => match self.modern_handshake(&conn).await {
                Ok(o) => o,
                // -32601 method-not-found (no discover) / unsupported version
                // → the server only speaks legacy. `-32022` is the current
                // UnsupportedProtocolVersionError code; `-32004` is the
                // pre-renumber value, tolerated for peers tracking an older
                // draft.
                Err(ClientError::Rpc(e))
                    if e.code == -32601 || e.code == -32022 || e.code == -32004 =>
                {
                    self.legacy_handshake(&conn).await?
                }
                Err(other) => return Err(other),
            },
        };

        // Precompute the modern `_meta` envelope (protocol version + identity)
        // merged into every request on the stateless draft path.
        let mut request_meta = Map::new();
        request_meta.insert(
            keys::PROTOCOL_VERSION.into(),
            json!(outcome.version.as_str()),
        );
        request_meta.insert(
            keys::CLIENT_INFO.into(),
            serde_json::to_value(&self.client_info).unwrap_or(Value::Null),
        );
        request_meta.insert(keys::CLIENT_CAPABILITIES.into(), self.capabilities.clone());

        Ok(Client {
            conn,
            version: outcome.version,
            server_info: outcome.server_info,
            server_capabilities: outcome.server_capabilities,
            instructions: outcome.instructions,
            request_meta,
            handler: self.handler.clone(),
            header_params: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// The modern, stateless handshake: a single `server/discover`.
    async fn modern_handshake(&self, conn: &Connection) -> ClientResult<Handshake> {
        let version = ProtocolVersion::LATEST;
        let mut meta = Map::new();
        meta.insert(keys::PROTOCOL_VERSION.into(), json!(version.as_str()));
        meta.insert(
            keys::CLIENT_INFO.into(),
            serde_json::to_value(&self.client_info).unwrap_or(Value::Null),
        );
        meta.insert(keys::CLIENT_CAPABILITIES.into(), self.capabilities.clone());
        let params = json!({ "_meta": Value::Object(meta) });

        let result = conn.request(request::DISCOVER, Some(params)).await?;
        Ok(Handshake::from_result(version, &result))
    }

    /// The legacy, stateful handshake: `initialize` then `notifications/initialized`.
    async fn legacy_handshake(&self, conn: &Connection) -> ClientResult<Handshake> {
        let version = ProtocolVersion::V2025_11_25;
        let params = json!({
            "protocolVersion": version.as_str(),
            "capabilities": self.capabilities,
            "clientInfo": serde_json::to_value(&self.client_info).unwrap_or(Value::Null),
        });
        let result = conn.request(request::INITIALIZE, Some(params)).await?;
        // Per the lifecycle spec, the client confirms readiness before issuing
        // further requests.
        conn.notify(notification::INITIALIZED, None).await?;
        Ok(Handshake::from_result(version, &result))
    }
}

/// The negotiated facts extracted from a handshake result.
struct Handshake {
    version: ProtocolVersion,
    server_info: Option<Implementation>,
    server_capabilities: Value,
    instructions: Option<String>,
}

impl Handshake {
    /// Pull `serverInfo` / `capabilities` / `instructions` out of an
    /// `initialize` or `server/discover` result; missing fields degrade
    /// gracefully rather than fail the handshake.
    ///
    /// The server identity lives at the top level on legacy `initialize`
    /// (`serverInfo`) and in the result `_meta` on the draft
    /// (`io.modelcontextprotocol/serverInfo` — the dedicated `DiscoverResult`
    /// field was removed upstream).
    fn from_result(version: ProtocolVersion, result: &Value) -> Self {
        Self {
            version,
            server_info: result
                .get("serverInfo")
                .or_else(|| {
                    result
                        .get("_meta")
                        .and_then(|m| m.get("io.modelcontextprotocol/serverInfo"))
                })
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
            server_capabilities: result
                .get("capabilities")
                .cloned()
                .unwrap_or(Value::Object(Map::new())),
            instructions: result
                .get("instructions")
                .and_then(Value::as_str)
                .map(String::from),
        }
    }
}

/// A connected, version-negotiated MCP client.
///
/// Cheaply [`Clone`]able (clones share the connection). All methods speak
/// version-stable [`neutral`] types; the client handles version stamping and
/// wire decoding internally, so calling code never branches on the protocol
/// version.
#[derive(Clone)]
pub struct Client {
    conn: Connection,
    version: ProtocolVersion,
    server_info: Option<Implementation>,
    server_capabilities: Value,
    instructions: Option<String>,
    request_meta: Map<String, Value>,
    handler: Option<Arc<dyn ClientHandler>>,
    /// Tool name → its `#[mcp_header]` parameter names, learned from `list_tools`.
    /// Drives transparent `Mcp-Param-*` mirroring on `call_tool`.
    header_params: Arc<Mutex<HashMap<String, Vec<HeaderParam>>>>,
}

impl Client {
    /// The protocol version negotiated at connect time.
    #[must_use]
    pub fn protocol_version(&self) -> &ProtocolVersion {
        &self.version
    }

    /// The server's advertised identity, if it provided one.
    #[must_use]
    pub fn server_info(&self) -> Option<&Implementation> {
        self.server_info.as_ref()
    }

    /// The server's advertised capabilities (raw JSON, version-shaped).
    #[must_use]
    pub fn server_capabilities(&self) -> &Value {
        &self.server_capabilities
    }

    /// The server's optional usage instructions.
    #[must_use]
    pub fn instructions(&self) -> Option<&str> {
        self.instructions.as_deref()
    }

    /// The underlying raw connection, for advanced/escape-hatch use.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Liveness check (`ping`). Version-agnostic — answered before version
    /// classification — so it carries no `_meta`.
    ///
    /// # Errors
    /// Propagates connection failure.
    pub async fn ping(&self) -> ClientResult<()> {
        // Routed through `versioned_request` so the HTTP transport can stamp
        // the required `MCP-Protocol-Version` header on the POST.
        self.versioned_request(request::PING, Map::new())
            .await
            .map(|_| ())
    }

    /// Issue a raw request for `method` with `params`, stamped with the
    /// negotiated protocol version and this client's declared capabilities
    /// (the same envelope the typed methods use). The escape hatch for methods
    /// the typed API doesn't model — notably extension methods such as the
    /// draft Tasks extension's `tasks/get`/`tasks/update`/`tasks/cancel`
    /// (SEP-2663). Returns the raw result `Value`.
    ///
    /// # Errors
    /// Propagates RPC failures (the server's JSON-RPC error).
    pub async fn request(&self, method: &str, params: Map<String, Value>) -> ClientResult<Value> {
        self.versioned_request(method, params).await
    }

    /// List the server's tools (one page; pass a `cursor` to continue).
    ///
    /// # Errors
    /// Propagates RPC and decode failures.
    pub async fn list_tools(&self, cursor: Option<&str>) -> ClientResult<neutral::ListToolsResult> {
        let v = self
            .versioned_request(request::TOOLS_LIST, list_params(cursor))
            .await?;
        let mut result: neutral::ListToolsResult =
            self.decode::<draft::ListToolsResult, legacy::ListToolsResult, _>(v)?;
        // Learn which params each tool marks `x-mcp-header` so `call_tool` can
        // mirror them transparently — and enforce the annotation constraints:
        // a tool whose annotations violate them MUST be rejected (excluded
        // from the result, with a warning), so one malformed definition
        // doesn't block the rest. (Last-seen page wins; tools paginate
        // cleanly.)
        let mut cache = self.header_params.lock().expect("header_params poisoned");
        result
            .tools
            .retain(|tool| match header_params_from_schema(&tool.input_schema) {
                Ok(headers) if headers.is_empty() => {
                    cache.remove(&tool.name);
                    true
                }
                Ok(headers) => {
                    cache.insert(tool.name.clone(), headers);
                    true
                }
                Err(reason) => {
                    tracing::warn!(
                        tool = %tool.name,
                        %reason,
                        "rejecting tool definition: invalid x-mcp-header annotation"
                    );
                    cache.remove(&tool.name);
                    false
                }
            });
        drop(cache);
        Ok(result)
    }

    /// Call a tool by name with an arguments object.
    ///
    /// # Errors
    /// Propagates RPC and decode failures. A *tool-level* failure is not an
    /// error here — it surfaces as `CallToolResult { is_error: true }`.
    pub async fn call_tool(
        &self,
        name: impl Into<String>,
        arguments: Map<String, Value>,
    ) -> ClientResult<neutral::CallToolResult> {
        let name = name.into();
        let params = self.tool_call_params(&name, &arguments);
        let v = match self.mrtr_request(request::TOOLS_CALL, params).await {
            // `-32020` HeaderMismatch: our mirror headers may be built from a
            // stale schema. Per the transports spec, refresh `tools/list`
            // (which rebuilds the header cache) and retry once.
            Err(ClientError::Rpc(e)) if e.code == -32020 => {
                tracing::warn!(
                    tool = %name,
                    "HeaderMismatch (-32020); refreshing tools/list and retrying once"
                );
                let _ = self.list_tools(None).await;
                let params = self.tool_call_params(&name, &arguments);
                self.mrtr_request(request::TOOLS_CALL, params).await?
            }
            other => other?,
        };
        self.decode::<draft::CallToolResult, legacy::CallToolResult, _>(v)
    }

    /// Build `tools/call` params, attaching the `x-mcp-header` mirror signal
    /// (header-name → encoded value, from the `list_tools` cache) for the HTTP
    /// transport to emit as `Mcp-Param-*` headers. Values stay in `arguments`
    /// — headers are copies, the body is authoritative. A parameter absent
    /// from `arguments` (or non-primitive) is simply not mirrored, per the
    /// extraction rule.
    fn tool_call_params(&self, name: &str, arguments: &Map<String, Value>) -> Map<String, Value> {
        let mut mirrors = Map::new();
        if let Some(headers) = self
            .header_params
            .lock()
            .expect("header_params poisoned")
            .get(name)
        {
            for param in headers {
                let mut value: Option<&Value> = None;
                for (i, segment) in param.path.iter().enumerate() {
                    value = if i == 0 {
                        arguments.get(segment)
                    } else {
                        value.and_then(|v| v.get(segment))
                    };
                }
                if let Some(rendered) = value.and_then(mcp_headers::render_argument) {
                    mirrors.insert(
                        param.header.clone(),
                        json!(mcp_headers::encode_value(&rendered)),
                    );
                }
            }
        }

        let mut params = Map::new();
        params.insert("name".into(), json!(name));
        params.insert("arguments".into(), Value::Object(arguments.clone()));
        if !mirrors.is_empty() {
            let mut meta = Map::new();
            meta.insert(HEADER_PARAMS_META_KEY.into(), Value::Object(mirrors));
            params.insert("_meta".into(), Value::Object(meta));
        }
        params
    }

    /// List the server's resources (one page; pass a `cursor` to continue).
    ///
    /// # Errors
    /// Propagates RPC and decode failures.
    pub async fn list_resources(
        &self,
        cursor: Option<&str>,
    ) -> ClientResult<neutral::ListResourcesResult> {
        let v = self
            .versioned_request(request::RESOURCES_LIST, list_params(cursor))
            .await?;
        self.decode::<draft::ListResourcesResult, legacy::ListResourcesResult, _>(v)
    }

    /// Read a resource by URI.
    ///
    /// # Errors
    /// Propagates RPC and decode failures.
    pub async fn read_resource(
        &self,
        uri: impl Into<String>,
    ) -> ClientResult<neutral::ReadResourceResult> {
        let mut params = Map::new();
        params.insert("uri".into(), json!(uri.into()));
        let v = self.mrtr_request(request::RESOURCES_READ, params).await?;
        self.decode::<draft::ReadResourceResult, legacy::ReadResourceResult, _>(v)
    }

    /// List the server's resource templates (one page; pass a `cursor` to continue).
    ///
    /// # Errors
    /// Propagates RPC and decode failures.
    pub async fn list_resource_templates(
        &self,
        cursor: Option<&str>,
    ) -> ClientResult<neutral::ListResourceTemplatesResult> {
        let v = self
            .versioned_request(request::RESOURCES_TEMPLATES_LIST, list_params(cursor))
            .await?;
        self.decode::<draft::ListResourceTemplatesResult, legacy::ListResourceTemplatesResult, _>(v)
    }

    /// List the server's prompts (one page; pass a `cursor` to continue).
    ///
    /// # Errors
    /// Propagates RPC and decode failures.
    pub async fn list_prompts(
        &self,
        cursor: Option<&str>,
    ) -> ClientResult<neutral::ListPromptsResult> {
        let v = self
            .versioned_request(request::PROMPTS_LIST, list_params(cursor))
            .await?;
        self.decode::<draft::ListPromptsResult, legacy::ListPromptsResult, _>(v)
    }

    /// Get a prompt by name with string arguments.
    ///
    /// # Errors
    /// Propagates RPC and decode failures.
    pub async fn get_prompt(
        &self,
        name: impl Into<String>,
        arguments: Map<String, Value>,
    ) -> ClientResult<neutral::GetPromptResult> {
        let mut params = Map::new();
        params.insert("name".into(), json!(name.into()));
        params.insert("arguments".into(), Value::Object(arguments));
        let v = self.mrtr_request(request::PROMPTS_GET, params).await?;
        self.decode::<draft::GetPromptResult, legacy::GetPromptResult, _>(v)
    }

    /// Request completion suggestions for a prompt/resource argument.
    ///
    /// `reference` and `argument` are passed through as the spec shapes them
    /// (`{ type, name }` / `{ name, value }`).
    ///
    /// # Errors
    /// Propagates RPC and decode failures.
    pub async fn complete(
        &self,
        reference: Value,
        argument: Value,
    ) -> ClientResult<neutral::CompleteResult> {
        let mut params = Map::new();
        params.insert("ref".into(), reference);
        params.insert("argument".into(), argument);
        let v = self
            .versioned_request(request::COMPLETION_COMPLETE, params)
            .await?;
        self.decode::<draft::CompleteResult, legacy::CompleteResult, _>(v)
    }

    /// Issue an MRTR-capable request (`tools/call`, `resources/read`,
    /// `prompts/get`), driving the draft input-required loop.
    ///
    /// On the modern path a server can answer `{ resultType: "input_required",
    /// inputRequests, requestState }`; this gathers each packaged request via the
    /// [`ClientHandler`] and re-issues the call with `inputResponses` + the echoed
    /// `requestState`, until a real result comes back. On the legacy path the
    /// server elicits inline (handled by the connection actor), so the first
    /// result is final and the loop runs exactly once.
    async fn mrtr_request(
        &self,
        method: &str,
        mut params: Map<String, Value>,
    ) -> ClientResult<Value> {
        for _ in 0..MAX_MRTR_ROUNDS {
            let result = self.versioned_request(method, params.clone()).await?;
            let input_required = result.get("resultType").and_then(Value::as_str)
                == Some(neutral::result_type::INPUT_REQUIRED);
            if !input_required {
                return Ok(result);
            }

            let handler = self.handler.as_deref().ok_or_else(|| {
                ClientError::Protocol(
                    "server requires input (MRTR) but the client has no handler".into(),
                )
            })?;

            // Answer each packaged input request, keyed exactly as the server sent.
            let mut responses = Map::new();
            if let Some(requests) = result.get("inputRequests").and_then(Value::as_object) {
                for (key, req) in requests {
                    let req_method = req
                        .get("method")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let req_params = req.get("params").cloned();
                    let answer = dispatch_server_request(handler, req_method, req_params)
                        .await
                        .map_err(|e| {
                            ClientError::Protocol(format!("input handler failed: {}", e.message))
                        })?;
                    responses.insert(key.clone(), answer);
                }
            }
            params.insert("inputResponses".into(), Value::Object(responses));
            // Carry the opaque resume state back verbatim, if present.
            if let Some(state) = result.get("requestState") {
                params.insert("requestState".into(), state.clone());
            }
        }
        Err(ClientError::Protocol(format!(
            "MRTR did not converge after {MAX_MRTR_ROUNDS} rounds"
        )))
    }

    /// Issue a request, stamping the modern `_meta` envelope when the negotiated
    /// version is the stateless draft (legacy carries identity in the session).
    /// Every request also carries the internal negotiated-version signal for
    /// the HTTP transport's `MCP-Protocol-Version` header (required on all
    /// post-negotiation requests by both versions' transports specs); other
    /// transports sanitize it at the server boundary.
    async fn versioned_request(
        &self,
        method: &str,
        mut params: Map<String, Value>,
    ) -> ClientResult<Value> {
        let meta = params
            .entry("_meta")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert(
                NEGOTIATED_VERSION_META_KEY.into(),
                json!(self.version.as_str()),
            );
            if self.version == ProtocolVersion::Draft {
                // Merge the version envelope into any existing `_meta` (e.g. the
                // `#[mcp_header]` mirror signal) rather than clobbering it.
                for (key, value) in &self.request_meta {
                    meta.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
        }
        self.conn.request(method, Some(Value::Object(params))).await
    }

    /// Decode a result into a [`neutral`] type via the negotiated version's wire
    /// shape: deserialize as `D` (draft) or `L` (legacy), then convert.
    fn decode<D, L, N>(&self, value: Value) -> ClientResult<N>
    where
        D: DeserializeOwned + Into<N>,
        L: DeserializeOwned + Into<N>,
    {
        if self.version == ProtocolVersion::Draft {
            serde_json::from_value::<D>(value)
                .map(Into::into)
                .map_err(|e| ClientError::Decode(e.to_string()))
        } else {
            serde_json::from_value::<L>(value)
                .map(Into::into)
                .map_err(|e| ClientError::Decode(e.to_string()))
        }
    }
}

/// Build `*/list` params from an optional pagination cursor.
fn list_params(cursor: Option<&str>) -> Map<String, Value> {
    let mut params = Map::new();
    if let Some(cursor) = cursor {
        params.insert("cursor".into(), json!(cursor));
    }
    params
}

/// One `x-mcp-header`-annotated tool parameter: the header-name portion
/// (mirrored as `Mcp-Param-{header}`) and the `properties` path to its value
/// in the call arguments.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeaderParam {
    header: String,
    path: Vec<String>,
}

/// Collect a tool's `x-mcp-header` annotations, enforcing the transports
/// spec's constraints. `Err` names the violation — a client using Streamable
/// HTTP MUST then reject the whole tool definition (exclude it from
/// `tools/list` and warn).
///
/// Constraints checked: the annotation is a string (the obsolete boolean form
/// is tolerated as "use the property name"), a valid RFC 9110 field-name
/// token, case-insensitively unique within the schema, applied only to
/// primitive `string`/`integer`/`boolean` parameters (never `number`), and
/// only on properties *statically reachable* through chains of `properties`
/// keys — an annotation under `items`, composition/conditional keywords,
/// `$ref`, or `$defs` invalidates the tool.
fn header_params_from_schema(input_schema: &Value) -> Result<Vec<HeaderParam>, String> {
    let mut found = Vec::new();
    scan(input_schema, true, &mut Vec::new(), &mut found)?;
    let mut seen = std::collections::HashSet::new();
    for param in &found {
        if !seen.insert(param.header.to_ascii_lowercase()) {
            return Err(format!(
                "duplicate x-mcp-header name {:?} (names are case-insensitively unique)",
                param.header
            ));
        }
    }
    return Ok(found);

    /// Walk every node; `reachable` is true only along root→`properties`→…
    /// chains. An `x-mcp-header` on any other node is invalid.
    fn scan(
        node: &Value,
        reachable: bool,
        path: &mut Vec<String>,
        found: &mut Vec<HeaderParam>,
    ) -> Result<(), String> {
        let Value::Object(map) = node else {
            if let Value::Array(items) = node {
                for item in items {
                    scan(item, false, path, found)?;
                }
            }
            return Ok(());
        };
        if let Some(annotation) = map.get("x-mcp-header") {
            if !reachable || path.is_empty() {
                return Err(format!(
                    "x-mcp-header at {:?} is not statically reachable via `properties` chains",
                    path.join(".")
                ));
            }
            let header = match annotation {
                Value::String(s) => s.clone(),
                // Obsolete boolean form (pre-string SEP-2243 revisions):
                // treat as "mirror under the property's own name".
                Value::Bool(true) => path.last().cloned().unwrap_or_default(),
                _ => {
                    return Err(format!(
                        "invalid x-mcp-header value at {:?}",
                        path.join(".")
                    ));
                }
            };
            if !mcp_headers::is_valid_header_name(&header) {
                return Err(format!(
                    "x-mcp-header {header:?} at {:?} is not a valid header-name token",
                    path.join(".")
                ));
            }
            let ty = map.get("type").and_then(Value::as_str);
            if !matches!(ty, Some("string" | "integer" | "boolean")) {
                return Err(format!(
                    "x-mcp-header at {:?} requires a primitive string/integer/boolean parameter",
                    path.join(".")
                ));
            }
            found.push(HeaderParam {
                header,
                path: path.clone(),
            });
        }
        for (key, child) in map {
            if key == "properties" && reachable {
                if let Value::Object(props) = child {
                    for (name, prop) in props {
                        path.push(name.clone());
                        scan(prop, true, path, found)?;
                        path.pop();
                    }
                }
            } else if key != "x-mcp-header" {
                // Everything else (items, oneOf/anyOf/allOf/not, if/then/else,
                // $defs, …) breaks static reachability for what's below it.
                scan(child, false, path, found)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod header_param_tests {
    use super::*;

    #[test]
    fn collects_string_annotations_and_nested_paths() {
        let schema = json!({
            "type": "object",
            "properties": {
                "region": { "type": "string", "x-mcp-header": "Region" },
                "options": {
                    "type": "object",
                    "properties": {
                        "tier": { "type": "integer", "x-mcp-header": "Tier" }
                    }
                },
                "query": { "type": "string" }
            }
        });
        let mut params = header_params_from_schema(&schema).unwrap();
        params.sort_by(|a, b| a.header.cmp(&b.header));
        assert_eq!(
            params,
            vec![
                HeaderParam {
                    header: "Region".into(),
                    path: vec!["region".into()],
                },
                HeaderParam {
                    header: "Tier".into(),
                    path: vec!["options".into(), "tier".into()],
                },
            ]
        );
    }

    #[test]
    fn tolerates_the_obsolete_boolean_form() {
        let schema = json!({
            "type": "object",
            "properties": { "region": { "type": "string", "x-mcp-header": true } }
        });
        let params = header_params_from_schema(&schema).unwrap();
        assert_eq!(params[0].header, "region");
    }

    #[test]
    fn rejects_constraint_violations() {
        // Not a tchar token.
        let bad_name = json!({
            "type": "object",
            "properties": { "a": { "type": "string", "x-mcp-header": "has space" } }
        });
        assert!(header_params_from_schema(&bad_name).is_err());

        // `number` is not permitted (integers only).
        let number_type = json!({
            "type": "object",
            "properties": { "a": { "type": "number", "x-mcp-header": "A" } }
        });
        assert!(header_params_from_schema(&number_type).is_err());

        // Case-insensitively duplicate names.
        let dupes = json!({
            "type": "object",
            "properties": {
                "a": { "type": "string", "x-mcp-header": "Region" },
                "b": { "type": "string", "x-mcp-header": "region" }
            }
        });
        assert!(header_params_from_schema(&dupes).is_err());

        // Not statically reachable: inside a composition keyword.
        let unreachable = json!({
            "type": "object",
            "properties": {
                "a": {
                    "oneOf": [
                        { "type": "string", "x-mcp-header": "A" },
                        { "type": "integer" }
                    ]
                }
            }
        });
        assert!(header_params_from_schema(&unreachable).is_err());

        // Not statically reachable: inside `items`.
        let in_items = json!({
            "type": "object",
            "properties": {
                "a": {
                    "type": "array",
                    "items": { "type": "string", "x-mcp-header": "A" }
                }
            }
        });
        assert!(header_params_from_schema(&in_items).is_err());
    }
}
