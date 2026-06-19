//! TurboMCP v4 Tasks extension — `io.modelcontextprotocol/tasks` (SEP-2663).
//!
//! The draft (`DRAFT-2026-v1`) moves Tasks out of the core protocol into an
//! **official extension**: a server may answer a `tools/call` with an
//! asynchronous *task handle* (`CreateTaskResult`, `resultType: "task"`)
//! instead of a final result, and the client polls `tasks/get` / drives input
//! via `tasks/update` / cancels via `tasks/cancel`. This crate owns those wire
//! types (the core draft schema defines none of them) and plugs into the
//! dispatcher through the [`Extension`] seam:
//!
//! ```ignore
//! use std::sync::Arc;
//! use turbomcp4_ext_tasks::TasksExtension;
//!
//! let dispatcher = my_server
//!     .into_server()
//!     .with_tools()
//!     .with_extension(Arc::new(TasksExtension::new()))
//!     .build();
//! ```
//!
//! Core Tasks for the legacy `2025-11-25` path (the different `tasks/list`/
//! `tasks/result` shape, session-scoped) is built into `turbomcp4-server` and
//! is unaffected by this extension — the dispatcher serves whichever the
//! negotiated version calls for.
//!
//! ## Capability negotiation (SEP-2663)
//!
//! Task creation is **server-directed**: the client signals support by
//! declaring the extension in its per-request capabilities
//! (`_meta.io.modelcontextprotocol/clientCapabilities.extensions`), and the
//! server decides per request whether to materialize a task. A client that has
//! not declared the extension capability gets `-32601` for `tasks/*` (enforced
//! by the dispatcher before [`TasksExtension::dispatch`] is reached).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use serde::Deserialize;
use turbomcp4_core::{JsonRpcError, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse, RequestId};
use turbomcp4_server::{Extension, ExtensionRequest};

pub mod wire;

/// The extension identifier, advertised under `server/discover`
/// `capabilities.extensions` and declared by clients to opt in.
pub const EXTENSION_ID: &str = "io.modelcontextprotocol/tasks";

/// Request methods this extension owns (SEP-2663 §Supported Methods).
pub mod methods {
    /// `tasks/get` — poll a task's current status (and, when terminal, its
    /// result or error inlined).
    pub const TASKS_GET: &str = "tasks/get";
    /// `tasks/update` — deliver `inputResponses` for an `input_required` task.
    pub const TASKS_UPDATE: &str = "tasks/update";
    /// `tasks/cancel` — request cancellation of an in-progress task.
    pub const TASKS_CANCEL: &str = "tasks/cancel";
}

const OWNED_METHODS: &[&str] = &[
    methods::TASKS_GET,
    methods::TASKS_UPDATE,
    methods::TASKS_CANCEL,
];

/// The draft Tasks extension (`io.modelcontextprotocol/tasks`).
///
/// Register it with `ServerBuilder::with_extension(Arc::new(TasksExtension::new()))`.
#[derive(Debug, Default)]
pub struct TasksExtension {
    // The task registry and augmentation policy land in Phase 9b; 9a wires the
    // extension seam (discover advertisement, capability gating, method
    // routing) with an empty registry, so every `tasks/*` lookup is a miss.
}

impl TasksExtension {
    /// Create the extension with an empty in-memory task registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// The `taskId`-only parameter shared by `tasks/get`/`tasks/update`/`tasks/cancel`.
#[derive(Deserialize)]
struct TaskIdParams {
    #[serde(rename = "taskId")]
    task_id: String,
}

/// Parse the request's `taskId` (`-32602` on an absent/invalid one, SEP-2663
/// §Error Handling).
fn parse_task_id(request: &JsonRpcRequest) -> Result<String, JsonRpcError> {
    request
        .params
        .as_ref()
        .and_then(|p| serde_json::from_value::<TaskIdParams>(p.clone()).ok())
        .map(|p| p.task_id)
        .ok_or_else(|| invalid_params("a `taskId` string is required"))
}

/// `-32602` (Invalid params).
fn invalid_params(message: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code: -32602,
        message: message.into(),
        data: None,
    }
}

/// `tasks/*` for a `taskId` no live task matches (`-32602`, SEP-2663).
fn task_not_found(task_id: &str) -> JsonRpcError {
    invalid_params(format!("unknown task: {task_id}"))
}

fn error(id: RequestId, err: JsonRpcError) -> JsonRpcMessage {
    JsonRpcResponse::error(id, err).into()
}

#[async_trait]
impl Extension for TasksExtension {
    fn id(&self) -> &'static str {
        EXTENSION_ID
    }

    fn methods(&self) -> &'static [&'static str] {
        OWNED_METHODS
    }

    async fn dispatch(&self, request: ExtensionRequest) -> JsonRpcMessage {
        let ExtensionRequest { request, .. } = request;
        let id = request.id.clone();

        // All three methods key off `taskId`; an absent/invalid one is `-32602`.
        let task_id = match parse_task_id(&request) {
            Ok(t) => t,
            Err(e) => return error(id, e),
        };

        // 9a: the registry is empty (task creation/augmentation lands in 9b),
        // so every lookup is a miss → `-32602` task-not-found.
        match request.method.as_str() {
            methods::TASKS_GET | methods::TASKS_UPDATE | methods::TASKS_CANCEL => {
                error(id, task_not_found(&task_id))
            }
            // The dispatcher only routes our declared methods here.
            other => error(
                id,
                JsonRpcError {
                    code: -32601,
                    message: format!("method not found: {other}"),
                    data: None,
                },
            ),
        }
    }
}
