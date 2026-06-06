//! Per-RPC context types.
//!
//! Each handler method receives a *typed* context that wraps the shared
//! [`RequestContext`] and conditionally exposes capabilities only valid for that
//! method. The load-bearing example (PLAN §4.4.1): `tools/call` may return an
//! `InputRequiredResult` (MRTR), so its context will carry a `ClientHandle`;
//! `tools/list` may not, so its context never will — calling `ctx.client` from
//! a `list_tools` handler is a *type error*, not a runtime check.
//!
//! Today every context is "plain" (it wraps a [`RequestContext`] and nothing
//! more), so they are generated from one [`plain_context!`] macro to keep them
//! in lockstep. In Phase 6 the MRTR-capable contexts ([`CallToolContext`],
//! [`ReadResourceContext`], [`GetPromptContext`] — SEP-2322) gain a
//! `ClientHandle` and a `request_state` accessor and graduate to hand-written
//! structs; the `*list*` contexts stay plain by design. Each is
//! `#[non_exhaustive]`, so that promotion is non-breaking.

use turbomcp4_core::RequestContext;

/// Define a per-RPC context that currently wraps only the shared
/// [`RequestContext`]. See the module docs for why these share one shape today.
macro_rules! plain_context {
    ($(#[$attr:meta])* $name:ident, $what:literal) => {
        #[doc = concat!("Context for `", $what, "`.")]
        ///
        /// Wraps the shared per-request metadata. (Gains a `ClientHandle` in
        /// Phase 6 only if the method is MRTR-capable.)
        $(#[$attr])*
        #[derive(Debug, Clone)]
        #[non_exhaustive]
        pub struct $name {
            /// Shared per-request metadata.
            pub base: RequestContext,
        }

        impl $name {
            /// Wrap a [`RequestContext`].
            #[must_use]
            pub fn new(base: RequestContext) -> Self {
                Self { base }
            }
        }
    };
}

plain_context!(ListToolsContext, "tools/list");
plain_context!(CallToolContext, "tools/call");
plain_context!(ListResourcesContext, "resources/list");
plain_context!(ListResourceTemplatesContext, "resources/templates/list");
plain_context!(ReadResourceContext, "resources/read");
plain_context!(ListPromptsContext, "prompts/list");
plain_context!(GetPromptContext, "prompts/get");
plain_context!(CompleteContext, "completion/complete");
