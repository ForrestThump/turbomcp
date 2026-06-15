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
//! # async fn f(transport: impl turbomcp4_service::Transport) -> turbomcp4_client::ClientResult<()> {
//! use turbomcp4_client::ClientBuilder;
//! let client = ClientBuilder::new("my-client", "1.0.0").connect(transport).await?;
//! let tools = client.list_tools(None).await?;
//! # Ok(()) }
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use turbomcp4_core::meta::keys;
use turbomcp4_core::{Implementation, ProtocolVersion};
use turbomcp4_protocol::methods::{notification, request};
use turbomcp4_protocol::neutral;
use turbomcp4_protocol::v2025_11_25::types as legacy;
use turbomcp4_protocol::v2026_draft::types as draft;
use turbomcp4_service::Transport;

use crate::connection::Connection;
use crate::error::{ClientError, ClientResult};
use crate::handler::{ClientHandler, dispatch_server_request};

/// Cap on MRTR re-issue rounds — a guard against a server that keeps answering
/// `input_required` forever.
const MAX_MRTR_ROUNDS: usize = 16;

/// Internal `_meta` key carrying the list of `#[mcp_header]` parameter names to
/// mirror as `Mcp-Param-*` headers. Consumed and stripped by the HTTP transport
/// (and sanitized server-side as an `io.turbomcp.internal/*` key on other
/// transports), so it never reaches a handler.
pub(crate) const HEADER_PARAMS_META_KEY: &str = "io.turbomcp.internal/headerParams";

/// How a [`Client`] decides which protocol version to speak.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConnectMode {
    /// Probe the modern (`server/discover`) path first; on `-32601`/`-32004`
    /// fall back to the legacy `initialize` handshake. The default.
    #[default]
    Auto,
    /// Force the modern, stateless `DRAFT-2026-v1` path (`server/discover`).
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
                // -32601 method-not-found (no discover) / -32004 unsupported
                // version → the server only speaks legacy.
                Err(ClientError::Rpc(e)) if e.code == -32601 || e.code == -32004 => {
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
    /// `initialize` or `server/discover` result (both share these field names);
    /// missing fields degrade gracefully rather than fail the handshake.
    fn from_result(version: ProtocolVersion, result: &Value) -> Self {
        Self {
            version,
            server_info: result
                .get("serverInfo")
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
    header_params: Arc<Mutex<HashMap<String, Vec<String>>>>,
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
        self.conn.request(request::PING, None).await.map(|_| ())
    }

    /// List the server's tools (one page; pass a `cursor` to continue).
    ///
    /// # Errors
    /// Propagates RPC and decode failures.
    pub async fn list_tools(&self, cursor: Option<&str>) -> ClientResult<neutral::ListToolsResult> {
        let v = self
            .versioned_request(request::TOOLS_LIST, list_params(cursor))
            .await?;
        let result: neutral::ListToolsResult =
            self.decode::<draft::ListToolsResult, legacy::ListToolsResult, _>(v)?;
        // Learn which params each tool marks `#[mcp_header]` so `call_tool` can
        // mirror them transparently. (Last-seen page wins; tools paginate cleanly.)
        let mut cache = self.header_params.lock().expect("header_params poisoned");
        for tool in &result.tools {
            let headers = header_param_names(&tool.input_schema);
            if headers.is_empty() {
                cache.remove(&tool.name);
            } else {
                cache.insert(tool.name.clone(), headers);
            }
        }
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
        // Mirror any `#[mcp_header]` params (learned from `list_tools`) to
        // `Mcp-Param-*` headers — values stay in `arguments`; the HTTP transport
        // lifts the signal into headers. No-op if the schema wasn't listed.
        let header_names: Vec<String> = self
            .header_params
            .lock()
            .expect("header_params poisoned")
            .get(&name)
            .map(|names| {
                names
                    .iter()
                    .filter(|n| arguments.contains_key(*n))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let mut params = Map::new();
        params.insert("name".into(), json!(name));
        params.insert("arguments".into(), Value::Object(arguments));
        if !header_names.is_empty() {
            let mut meta = Map::new();
            meta.insert(HEADER_PARAMS_META_KEY.into(), json!(header_names));
            params.insert("_meta".into(), Value::Object(meta));
        }
        let v = self.mrtr_request(request::TOOLS_CALL, params).await?;
        self.decode::<draft::CallToolResult, legacy::CallToolResult, _>(v)
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
    async fn versioned_request(
        &self,
        method: &str,
        mut params: Map<String, Value>,
    ) -> ClientResult<Value> {
        if self.version == ProtocolVersion::Draft2026V1 {
            // Merge the version envelope into any existing `_meta` (e.g. the
            // `#[mcp_header]` mirror signal) rather than clobbering it.
            let meta = params
                .entry("_meta")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(meta) = meta.as_object_mut() {
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
        if self.version == ProtocolVersion::Draft2026V1 {
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

/// The names of a tool's `#[mcp_header]`-marked parameters — its input-schema
/// properties flagged `"x-mcp-header": true` (set by the `#[mcp_header]` macro
/// marker via `mark_mcp_header`).
fn header_param_names(input_schema: &Value) -> Vec<String> {
    let Some(properties) = input_schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    properties
        .iter()
        .filter(|(_, schema)| schema.get("x-mcp-header").and_then(Value::as_bool) == Some(true))
        .map(|(name, _)| name.clone())
        .collect()
}
