//! Wire method and notification names, as string constants.
//!
//! Centralizing these keeps the `MethodRouter` and per-version handlers from
//! re-typing string literals (and silently disagreeing). Names that exist in
//! only one version are noted.

/// Request method names.
pub mod request {
    /// `server/discover` — stateless capability discovery (`DRAFT-2026-v1`).
    pub const DISCOVER: &str = "server/discover";
    /// `initialize` — stateful handshake (`2025-11-25` and earlier).
    pub const INITIALIZE: &str = "initialize";
    /// `ping` — liveness probe (core in `2025-11-25`).
    pub const PING: &str = "ping";
    /// `tools/list` — enumerate tools.
    pub const TOOLS_LIST: &str = "tools/list";
    /// `tools/call` — invoke a tool.
    pub const TOOLS_CALL: &str = "tools/call";
    /// `resources/list` — enumerate resources.
    pub const RESOURCES_LIST: &str = "resources/list";
    /// `resources/templates/list` — enumerate resource templates.
    pub const RESOURCES_TEMPLATES_LIST: &str = "resources/templates/list";
    /// `resources/read` — read a resource.
    pub const RESOURCES_READ: &str = "resources/read";
    /// `prompts/list` — enumerate prompts.
    pub const PROMPTS_LIST: &str = "prompts/list";
    /// `prompts/get` — render a prompt.
    pub const PROMPTS_GET: &str = "prompts/get";
    /// `completion/complete` — argument autocompletion.
    pub const COMPLETION_COMPLETE: &str = "completion/complete";
    /// `tasks/list` — enumerate tasks (core in `2025-11-25`; extension in draft).
    pub const TASKS_LIST: &str = "tasks/list";
    /// `tasks/get` — poll a task's status (core in `2025-11-25`).
    pub const TASKS_GET: &str = "tasks/get";
    /// `tasks/cancel` — request cancellation of a task (core in `2025-11-25`).
    pub const TASKS_CANCEL: &str = "tasks/cancel";
    /// `tasks/result` — retrieve a task's final result, blocking until the task
    /// reaches a terminal status (core in `2025-11-25`).
    pub const TASKS_RESULT: &str = "tasks/result";
}

/// Notification method names (no response).
pub mod notification {
    /// `notifications/initialized` — client finished initializing (stateful).
    pub const INITIALIZED: &str = "notifications/initialized";
    /// `notifications/cancelled` — a previously issued request is cancelled.
    pub const CANCELLED: &str = "notifications/cancelled";
    /// `notifications/tasks/status` — a task's status changed (optional per
    /// spec; requestors must poll `tasks/get` regardless).
    pub const TASKS_STATUS: &str = "notifications/tasks/status";
}
