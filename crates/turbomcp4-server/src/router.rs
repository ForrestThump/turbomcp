//! [`MethodRouter`]: the capability registry that backs a [`VersionDispatcher`].
//!
//! The router stores one type-erased handler closure per supported method.
//! Registration methods (`with_tools`, …) are bounded on the corresponding
//! capability trait, so a server can only register `tools/*` if it actually
//! implements [`WithTools`] — yet the *dispatcher* needs only `McpServerCore`,
//! because the trait bound is erased into the stored closure (axum's pattern).
//!
//! Two consequences fall out for free:
//! - Advertised capabilities are derived from what's registered ([`has_tools`]),
//!   so they cannot drift from the handlers.
//! - A future macro (`#[server]`, Phase 3) registers exactly the capabilities
//!   the user implemented, with no per-server boilerplate here.
//!
//! [`VersionDispatcher`]: crate::VersionDispatcher
//! [`has_tools`]: MethodRouter::has_tools

use futures::future::BoxFuture;

use turbomcp4_core::McpResult;
use turbomcp4_protocol::neutral;

use crate::context::{CallToolContext, ListToolsContext};
use crate::traits::{McpServerCore, WithTools};

type ListToolsHandler<S> = Box<
    dyn Fn(S, ListToolsContext) -> BoxFuture<'static, McpResult<neutral::ListToolsResult>>
        + Send
        + Sync,
>;

type CallToolHandler<S> = Box<
    dyn Fn(
            S,
            CallToolContext,
            neutral::CallToolParams,
        ) -> BoxFuture<'static, McpResult<neutral::CallToolResult>>
        + Send
        + Sync,
>;

/// Per-method handler table, generic over the server type `S`.
pub struct MethodRouter<S> {
    list_tools: Option<ListToolsHandler<S>>,
    call_tool: Option<CallToolHandler<S>>,
}

impl<S> Default for MethodRouter<S> {
    fn default() -> Self {
        Self {
            list_tools: None,
            call_tool: None,
        }
    }
}

impl<S: McpServerCore> MethodRouter<S> {
    /// An empty router — no capabilities registered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the `tools/*` methods. Available only when `S: WithTools`; the
    /// bound is erased into the stored closures so the dispatcher stays generic
    /// over plain `McpServerCore`.
    #[must_use]
    pub fn with_tools(mut self) -> Self
    where
        S: WithTools,
    {
        self.list_tools = Some(Box::new(|server: S, ctx: ListToolsContext| {
            Box::pin(async move { server.list_tools(&ctx).await })
        }));
        self.call_tool = Some(Box::new(
            |server: S, ctx: CallToolContext, params: neutral::CallToolParams| {
                Box::pin(async move { server.call_tool(&ctx, params).await })
            },
        ));
        self
    }

    /// Whether `tools/*` is served (drives capability advertisement).
    #[must_use]
    pub fn has_tools(&self) -> bool {
        self.list_tools.is_some()
    }

    /// Invoke the registered `tools/list` handler, if any.
    pub(crate) fn dispatch_list_tools(
        &self,
        server: S,
        ctx: ListToolsContext,
    ) -> Option<BoxFuture<'static, McpResult<neutral::ListToolsResult>>> {
        self.list_tools.as_ref().map(|h| h(server, ctx))
    }

    /// Invoke the registered `tools/call` handler, if any.
    pub(crate) fn dispatch_call_tool(
        &self,
        server: S,
        ctx: CallToolContext,
        params: neutral::CallToolParams,
    ) -> Option<BoxFuture<'static, McpResult<neutral::CallToolResult>>> {
        self.call_tool.as_ref().map(|h| h(server, ctx, params))
    }
}
