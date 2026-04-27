//! Backend connector for proxy
//!
//! Manages connection to the backend MCP server using turbomcp-client.
//! Supports multiple backend transport types (STDIO, HTTP, WebSocket).

use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
#[cfg(unix)]
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tracing::{debug, info};
use turbomcp_client::Client;
use turbomcp_protocol::types::{GetPromptResult, Prompt, ReadResourceResult, Resource, Tool};
use turbomcp_protocol::{Error, PROTOCOL_VERSION};
#[cfg(unix)]
use turbomcp_transport::UnixTransport;
use turbomcp_transport::{
    ChildProcessConfig, ChildProcessTransport, TcpTransport, Transport,
    WebSocketBidirectionalConfig, WebSocketBidirectionalTransport,
    streamable_http_client::{StreamableHttpClientConfig, StreamableHttpClientTransport},
};

use crate::error::{ProxyError, ProxyResult};
use crate::introspection::{
    EmptyCapability, LoggingCapability, PromptSpec, PromptsCapability, ResourceSpec,
    ResourcesCapability, ServerCapabilities, ServerInfo, ServerSpec, ToolInputSchema, ToolSpec,
    ToolsCapability,
};

/// Type alias for async result futures used in `ProxyClient` trait (v3.0: `McpError` not boxed)
type ClientFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

/// Trait that abstracts over MCP client operations with explicit Future return types
///
/// This trait enables type-erased client handling, allowing the backend connector to work
/// with any transport type without needing to match on a transport-specific enum.
/// This eliminates code duplication from macro-based dispatch patterns.
pub trait ProxyClient: Send + Sync {
    /// List all available tools
    fn list_tools(&self) -> ClientFuture<'_, Vec<Tool>>;

    /// List all available resources
    fn list_resources(&self) -> ClientFuture<'_, Vec<Resource>>;

    /// List all available prompts
    fn list_prompts(&self) -> ClientFuture<'_, Vec<Prompt>>;

    /// Call a specific tool with arguments
    fn call_tool(
        &self,
        name: &str,
        arguments: Option<HashMap<String, Value>>,
    ) -> ClientFuture<'_, Value>;

    /// Read a specific resource
    fn read_resource(&self, uri: &str) -> ClientFuture<'_, ReadResourceResult>;

    /// Get a specific prompt with arguments
    fn get_prompt(
        &self,
        name: &str,
        arguments: Option<HashMap<String, Value>>,
    ) -> ClientFuture<'_, GetPromptResult>;
}

/// Concrete implementation of `ProxyClient` for a specific transport type
struct ConcreteProxyClient<T: Transport + 'static> {
    client: Arc<Client<T>>,
}

impl<T: Transport + 'static> ProxyClient for ConcreteProxyClient<T> {
    fn list_tools(&self) -> ClientFuture<'_, Vec<Tool>> {
        let client = self.client.clone();
        Box::pin(async move { client.list_tools().await })
    }

    fn list_resources(&self) -> ClientFuture<'_, Vec<Resource>> {
        let client = self.client.clone();
        Box::pin(async move { client.list_resources().await })
    }

    fn list_prompts(&self) -> ClientFuture<'_, Vec<Prompt>> {
        let client = self.client.clone();
        Box::pin(async move { client.list_prompts().await })
    }

    fn call_tool(
        &self,
        name: &str,
        arguments: Option<HashMap<String, Value>>,
    ) -> ClientFuture<'_, Value> {
        let client = self.client.clone();
        let name = name.to_string();
        Box::pin(async move {
            let result = client.call_tool(&name, arguments, None).await?;
            // Serialize CallToolResult to JSON for proxy transport
            Ok(serde_json::to_value(result)?)
        })
    }

    fn read_resource(&self, uri: &str) -> ClientFuture<'_, ReadResourceResult> {
        let client = self.client.clone();
        let uri = uri.to_string();
        Box::pin(async move { client.read_resource(&uri).await })
    }

    fn get_prompt(
        &self,
        name: &str,
        arguments: Option<HashMap<String, Value>>,
    ) -> ClientFuture<'_, GetPromptResult> {
        let client = self.client.clone();
        let name = name.to_string();
        Box::pin(async move { client.get_prompt(&name, arguments).await })
    }
}

/// Backend transport type
#[derive(Debug, Clone)]
pub enum BackendTransport {
    /// Standard I/O (subprocess)
    Stdio {
        /// Command to execute
        command: String,
        /// Command arguments
        args: Vec<String>,
        /// Working directory
        working_dir: Option<String>,
    },
    /// HTTP with Server-Sent Events
    Http {
        /// Base URL
        url: String,
        /// Optional path on the upstream MCP server where requests are `POST`ed.
        /// Defaults to `/mcp` when `None`. Servers that mount MCP at a custom
        /// location (e.g. `/api/mcp`) must set this to be reachable.
        endpoint_path: Option<String>,
        /// Optional authentication token, wrapped in [`SecretString`] so it is
        /// redacted from `Debug` output and zeroed on drop.
        auth_token: Option<SecretString>,
    },
    /// TCP bidirectional communication
    Tcp {
        /// Host or IP address
        host: String,
        /// Port number
        port: u16,
    },
    /// Unix domain socket
    #[cfg(unix)]
    Unix {
        /// Socket file path
        path: String,
    },
    /// WebSocket bidirectional
    WebSocket {
        /// WebSocket URL
        url: String,
    },
}

/// Logging-safe discriminant for [`BackendTransport`]. Avoids leaking the
/// inner fields (notably `Http.auth_token`) into structured log output.
fn backend_transport_kind(t: &BackendTransport) -> &'static str {
    match t {
        BackendTransport::Stdio { .. } => "stdio",
        BackendTransport::Http { .. } => "http",
        BackendTransport::Tcp { .. } => "tcp",
        #[cfg(unix)]
        BackendTransport::Unix { .. } => "unix",
        BackendTransport::WebSocket { .. } => "websocket",
    }
}

/// Backend configuration
#[derive(Debug, Clone)]
pub struct BackendConfig {
    /// Transport configuration
    pub transport: BackendTransport,

    /// Client name for initialization
    pub client_name: String,

    /// Client version for initialization
    pub client_version: String,
}

/// Backend connector wrapping turbomcp-client
///
/// Manages the connection to the backend MCP server and provides
/// type-safe methods for all MCP protocol operations.
#[derive(Clone)]
pub struct BackendConnector {
    /// The underlying turbomcp client (transport-agnostic, trait object)
    client: Arc<dyn ProxyClient>,

    /// Backend configuration
    #[allow(dead_code)] // Kept for future use and debugging
    config: Arc<BackendConfig>,

    /// Cached server spec (from introspection)
    spec: Arc<tokio::sync::Mutex<Option<ServerSpec>>>,

    /// Real `InitializeResult` captured during connect.
    ///
    /// Stored so `introspect_via_client` can return the upstream's actual
    /// server info and capabilities instead of synthesizing a hardcoded
    /// surface (audit CRIT — proxy was lying to clients about what the
    /// upstream supports).
    init_result: Arc<turbomcp_client::InitializeResult>,
}

impl std::fmt::Debug for BackendConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendConnector")
            .field("config", &self.config)
            .field("spec", &"<Mutex>")
            .finish_non_exhaustive()
    }
}

impl BackendConnector {
    /// Create a new backend connector
    ///
    /// # Arguments
    ///
    /// * `config` - Backend configuration
    ///
    /// # Returns
    ///
    /// A connected backend connector ready for requests
    ///
    /// # Errors
    ///
    /// Returns `ProxyError` if the backend fails to initialize, connect, or if the transport type is not supported.
    ///
    /// # Panics
    ///
    /// Panics if "127.0.0.1:0" cannot be parsed as a `SocketAddr` (should never happen as it's a valid address).
    #[allow(clippy::too_many_lines)]
    pub async fn new(config: BackendConfig) -> ProxyResult<Self> {
        // Don't log `config.transport` directly: `BackendTransport::Http`
        // wraps an `Option<SecretString>` whose Debug redacts the bearer, but
        // historic versions of this struct logged the bearer at INFO level.
        // Log only the discriminant so credential leaks remain impossible
        // even if the type drifts back to a printable inner type.
        info!(
            transport_kind = backend_transport_kind(&config.transport),
            "Creating backend connector"
        );

        // Create client based on transport type. Each arm yields both the
        // type-erased proxy client and the upstream's real `InitializeResult`.
        let (client, init_result): (Arc<dyn ProxyClient>, _) = match &config.transport {
            BackendTransport::Stdio {
                command,
                args,
                working_dir,
            } => {
                let process_config = ChildProcessConfig {
                    command: command.clone(),
                    args: args.clone(),
                    working_directory: working_dir.clone(),
                    environment: None,
                    ..Default::default()
                };

                let transport = ChildProcessTransport::new(process_config);

                // Connect the transport
                transport.connect().await.map_err(|e| {
                    ProxyError::backend(format!("Failed to connect to subprocess: {e}"))
                })?;

                debug!("STDIO backend connected: {} {:?}", command, args);

                // Create and initialize client
                let client = Client::new(transport);
                let init_result = client.initialize().await.map_err(|e| {
                    ProxyError::backend(format!("Failed to initialize backend: {e}"))
                })?;

                let proxy_client: Arc<dyn ProxyClient> = Arc::new(ConcreteProxyClient {
                    client: Arc::new(client),
                });
                (proxy_client, init_result)
            }

            BackendTransport::Http {
                url,
                endpoint_path,
                auth_token,
            } => {
                // Expose the secret only at this single egress point — the
                // value flows directly into the HTTP transport's bearer header.
                // Anywhere else (Debug, logs, deserialized config) it stays
                // wrapped in SecretString.
                let http_config = StreamableHttpClientConfig {
                    base_url: url.clone(),
                    endpoint_path: endpoint_path.clone().unwrap_or_else(|| "/mcp".to_string()),
                    timeout: std::time::Duration::from_secs(30),
                    auth_token: auth_token.as_ref().map(|s| s.expose_secret().to_string()),
                    ..Default::default()
                };

                let transport = StreamableHttpClientTransport::new(http_config).map_err(|e| {
                    ProxyError::backend(format!("Failed to build HTTP transport: {e}"))
                })?;

                // Connect the transport
                transport.connect().await.map_err(|e| {
                    ProxyError::backend(format!("Failed to connect to HTTP backend: {e}"))
                })?;

                debug!("HTTP backend connected: {}", url);

                // Create and initialize client
                let client = Client::new(transport);
                let init_result = client.initialize().await.map_err(|e| {
                    ProxyError::backend(format!("Failed to initialize backend: {e}"))
                })?;

                let proxy_client: Arc<dyn ProxyClient> = Arc::new(ConcreteProxyClient {
                    client: Arc::new(client),
                });
                (proxy_client, init_result)
            }

            BackendTransport::Tcp { host, port } => {
                let addr = format!("{host}:{port}")
                    .parse::<SocketAddr>()
                    .map_err(|e| ProxyError::backend(format!("Invalid TCP address: {e}")))?;

                let transport =
                    TcpTransport::new_client(SocketAddr::from(([127, 0, 0, 1], 0)), addr);

                // Connect the transport
                transport.connect().await.map_err(|e| {
                    ProxyError::backend(format!("Failed to connect to TCP backend: {e}"))
                })?;

                debug!("TCP backend connected: {}:{}", host, port);

                // Create and initialize client
                let client = Client::new(transport);
                let init_result = client.initialize().await.map_err(|e| {
                    ProxyError::backend(format!("Failed to initialize backend: {e}"))
                })?;

                let proxy_client: Arc<dyn ProxyClient> = Arc::new(ConcreteProxyClient {
                    client: Arc::new(client),
                });
                (proxy_client, init_result)
            }

            #[cfg(unix)]
            BackendTransport::Unix { path } => {
                let socket_path = PathBuf::from(path);

                let transport = UnixTransport::new_client(socket_path.clone());

                // Connect the transport
                transport.connect().await.map_err(|e| {
                    ProxyError::backend(format!("Failed to connect to Unix socket: {e}"))
                })?;

                debug!("Unix socket backend connected: {}", path);

                // Create and initialize client
                let client = Client::new(transport);
                let init_result = client.initialize().await.map_err(|e| {
                    ProxyError::backend(format!("Failed to initialize backend: {e}"))
                })?;

                let proxy_client: Arc<dyn ProxyClient> = Arc::new(ConcreteProxyClient {
                    client: Arc::new(client),
                });
                (proxy_client, init_result)
            }

            BackendTransport::WebSocket { url } => {
                let ws_config = WebSocketBidirectionalConfig {
                    url: Some(url.clone()),
                    ..Default::default()
                };

                let transport = WebSocketBidirectionalTransport::new(ws_config)
                    .await
                    .map_err(|e| {
                        ProxyError::backend(format!("Failed to connect to WebSocket: {e}"))
                    })?;

                debug!("WebSocket backend connected: {}", url);

                // Create and initialize client
                let client = Client::new(transport);
                let init_result = client.initialize().await.map_err(|e| {
                    ProxyError::backend(format!("Failed to initialize backend: {e}"))
                })?;

                let proxy_client: Arc<dyn ProxyClient> = Arc::new(ConcreteProxyClient {
                    client: Arc::new(client),
                });
                (proxy_client, init_result)
            }
        };

        info!("Backend initialized successfully");

        Ok(Self {
            client,
            config: Arc::new(config),
            spec: Arc::new(tokio::sync::Mutex::new(None)),
            init_result: Arc::new(init_result),
        })
    }

    /// Introspect the backend server
    ///
    /// Discovers all capabilities (tools, resources, prompts) and caches
    /// the result for use by the frontend server.
    ///
    /// # Errors
    ///
    /// Returns `ProxyError` if the introspection fails or the server capabilities cannot be determined.
    pub async fn introspect(&self) -> ProxyResult<ServerSpec> {
        debug!("Introspecting backend server");

        // Perform introspection via the client
        let spec = self.introspect_via_client().await?;

        // Cache the spec
        *self.spec.lock().await = Some(spec.clone());

        info!(
            "Backend introspection complete: {} tools, {} resources, {} prompts",
            spec.tools.len(),
            spec.resources.len(),
            spec.prompts.len()
        );

        Ok(spec)
    }

    /// Introspect via client methods.
    ///
    /// Uses the real `InitializeResult` captured during connect so the proxy
    /// advertises the upstream's actual capabilities and server info — not a
    /// hardcoded guess (audit CRIT). Tool/resource/prompt list calls remain so
    /// the spec exposes a static enumeration; for liveness against
    /// `notifications/{tools,resources,prompts}/list_changed`, see the proxy
    /// audit's MED on cache invalidation.
    async fn introspect_via_client(&self) -> ProxyResult<ServerSpec> {
        // List tools
        let tools = self
            .client
            .list_tools()
            .await
            .map_err(|e| ProxyError::backend(format!("Failed to list tools: {e}")))?;

        // List resources
        let resources = self
            .client
            .list_resources()
            .await
            .map_err(|e| ProxyError::backend(format!("Failed to list resources: {e}")))?;

        // List prompts
        let prompts = self
            .client
            .list_prompts()
            .await
            .map_err(|e| ProxyError::backend(format!("Failed to list prompts: {e}")))?;

        // Use the real InitializeResult captured at connect time.
        let server_info = ServerInfo {
            name: self.init_result.server_info.name.clone(),
            version: self.init_result.server_info.version.clone(),
            title: self.init_result.server_info.title.clone(),
        };

        Ok(ServerSpec {
            server_info,
            // The client crate's `InitializeResult` doesn't keep `protocol_version`;
            // echo our compile-time constant. Spec-strict negotiation happens during
            // the upstream `Client::initialize` itself.
            protocol_version: PROTOCOL_VERSION.to_string(),
            capabilities: Self::convert_capabilities(&self.init_result.server_capabilities),
            tools: Self::convert_tools(tools),
            resources: Self::convert_resources(resources),
            prompts: Self::convert_prompts(prompts),
            resource_templates: Vec::new(),
            instructions: None,
        })
    }

    /// Convert `turbomcp_protocol::types::ServerCapabilities` (from upstream
    /// `InitializeResult`) into the proxy's `spec::ServerCapabilities` shape.
    /// Mirrors the policy in `introspection::introspector::extract_capabilities`.
    fn convert_capabilities(
        caps: &turbomcp_protocol::types::ServerCapabilities,
    ) -> ServerCapabilities {
        ServerCapabilities {
            logging: caps.logging.as_ref().map(|_| LoggingCapability {}),
            completions: caps.completions.as_ref().map(|_| EmptyCapability {}),
            prompts: caps.prompts.as_ref().map(|p| PromptsCapability {
                list_changed: p.list_changed,
            }),
            resources: caps.resources.as_ref().map(|r| ResourcesCapability {
                subscribe: r.subscribe,
                list_changed: r.list_changed,
            }),
            tools: caps.tools.as_ref().map(|t| ToolsCapability {
                list_changed: t.list_changed,
            }),
            experimental: caps.experimental.clone(),
        }
    }

    fn convert_tools(tools: Vec<Tool>) -> Vec<ToolSpec> {
        tools
            .into_iter()
            .map(|t| {
                let mut additional = HashMap::new();
                if let Some(ref additional_props) = t.input_schema.additional_properties {
                    additional.insert("additionalProperties".to_string(), additional_props.clone());
                }
                for (key, value) in &t.input_schema.extra_keywords {
                    additional.insert(key.clone(), value.clone());
                }
                let schema_type = t
                    .input_schema
                    .schema_type
                    .as_ref()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "object".to_string());
                let properties = t.input_schema.properties_as_object().map(|obj| {
                    obj.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<std::collections::HashMap<_, _>>()
                });
                ToolSpec {
                    name: t.name,
                    title: t.title,
                    description: t.description,
                    input_schema: ToolInputSchema {
                        schema_type,
                        properties,
                        required: t.input_schema.required,
                        additional,
                    },
                    output_schema: None,
                    annotations: None,
                }
            })
            .collect()
    }

    fn convert_resources(resources: Vec<Resource>) -> Vec<ResourceSpec> {
        resources
            .into_iter()
            .map(|r| ResourceSpec {
                uri: r.uri.clone(),
                name: r.name,
                title: None,
                description: r.description,
                mime_type: r.mime_type,
                size: None,
                annotations: None,
            })
            .collect()
    }

    fn convert_prompts(prompts: Vec<Prompt>) -> Vec<PromptSpec> {
        prompts
            .into_iter()
            .map(|p| {
                let arguments = p
                    .arguments
                    .unwrap_or_default()
                    .into_iter()
                    .map(|a| crate::introspection::PromptArgument {
                        name: a.name,
                        title: None,
                        description: a.description,
                        required: a.required,
                    })
                    .collect();
                PromptSpec {
                    name: p.name,
                    title: None,
                    description: p.description,
                    arguments,
                }
            })
            .collect()
    }

    /// Get cached server spec
    #[must_use]
    pub async fn spec(&self) -> Option<ServerSpec> {
        self.spec.lock().await.clone()
    }

    /// Call a tool on the backend
    ///
    /// # Errors
    ///
    /// Returns `ProxyError` if the tool call fails or the tool is not found.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<HashMap<String, Value>>,
    ) -> ProxyResult<Value> {
        debug!("Calling backend tool: {}", name);

        self.client.call_tool(name, arguments).await.map_err(|e| {
            ProxyError::backend_with_code(format!("Tool call failed: {e}"), e.jsonrpc_error_code())
        })
    }

    /// List tools from the backend
    ///
    /// # Errors
    ///
    /// Returns `ProxyError` if listing tools fails.
    pub async fn list_tools(&self) -> ProxyResult<Vec<Tool>> {
        self.client.list_tools().await.map_err(|e| {
            ProxyError::backend_with_code(
                format!("Failed to list tools: {e}"),
                e.jsonrpc_error_code(),
            )
        })
    }

    /// List resources from the backend
    ///
    /// # Errors
    ///
    /// Returns `ProxyError` if listing resources fails.
    pub async fn list_resources(&self) -> ProxyResult<Vec<Resource>> {
        self.client.list_resources().await.map_err(|e| {
            ProxyError::backend_with_code(
                format!("Failed to list resources: {e}"),
                e.jsonrpc_error_code(),
            )
        })
    }

    /// Read a resource from the backend
    ///
    /// # Errors
    ///
    /// Returns `ProxyError` if reading the resource fails or the resource is not found.
    pub async fn read_resource(&self, uri: &str) -> ProxyResult<ReadResourceResult> {
        self.client.read_resource(uri).await.map_err(|e| {
            ProxyError::backend_with_code(
                format!("Failed to read resource: {e}"),
                e.jsonrpc_error_code(),
            )
        })
    }

    /// List prompts from the backend
    ///
    /// # Errors
    ///
    /// Returns `ProxyError` if listing prompts fails.
    pub async fn list_prompts(&self) -> ProxyResult<Vec<Prompt>> {
        self.client.list_prompts().await.map_err(|e| {
            ProxyError::backend_with_code(
                format!("Failed to list prompts: {e}"),
                e.jsonrpc_error_code(),
            )
        })
    }

    /// Get a prompt from the backend
    ///
    /// # Errors
    ///
    /// Returns `ProxyError` if getting the prompt fails or the prompt is not found.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<HashMap<String, Value>>,
    ) -> ProxyResult<turbomcp_protocol::types::GetPromptResult> {
        self.client.get_prompt(name, arguments).await.map_err(|e| {
            ProxyError::backend_with_code(
                format!("Failed to get prompt: {e}"),
                e.jsonrpc_error_code(),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_config_creation() {
        let config = BackendConfig {
            transport: BackendTransport::Stdio {
                command: "python".to_string(),
                args: vec!["server.py".to_string()],
                working_dir: None,
            },
            client_name: "test-proxy".to_string(),
            client_version: "1.0.0".to_string(),
        };

        assert_eq!(config.client_name, "test-proxy");
        assert_eq!(config.client_version, "1.0.0");
    }

    #[tokio::test]
    #[ignore = "Requires building manual_server example via cargo run"]
    async fn test_backend_connector_with_echo() {
        // This test requires the manual_server example to be built
        let config = BackendConfig {
            transport: BackendTransport::Stdio {
                command: "cargo".to_string(),
                args: vec![
                    "run".to_string(),
                    "--package".to_string(),
                    "turbomcp-server".to_string(),
                    "--example".to_string(),
                    "manual_server".to_string(),
                ],
                working_dir: Some("/Users/nickpaterno/work/turbomcp".to_string()),
            },
            client_name: "test-proxy".to_string(),
            client_version: "1.0.0".to_string(),
        };

        let result = BackendConnector::new(config).await;
        if let Ok(backend) = result {
            // Try introspection
            let spec = backend.introspect().await;
            if let Ok(spec) = spec {
                assert!(!spec.tools.is_empty(), "Should have at least one tool");
            }
        }
    }
}
