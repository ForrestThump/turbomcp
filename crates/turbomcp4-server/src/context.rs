//! Per-RPC context types.
//!
//! Each handler method receives a *typed* context that wraps the shared
//! [`RequestContext`] and conditionally exposes capabilities only valid for that
//! method. The load-bearing example (PLAN §4.4.1): `tools/call` may return an
//! `InputRequiredResult` (MRTR), so its context carries a [`ClientHandle`];
//! `tools/list` may not, so its context never will — calling `ctx.client` from
//! a `list_tools` handler is a *type error*, not a runtime check.
//!
//! The MRTR-capable contexts ([`CallToolContext`], [`ReadResourceContext`],
//! [`GetPromptContext`]) are exactly the three RPCs SEP-2322 allows
//! `InputRequiredResult` on; the `*list*` and `complete` contexts stay plain
//! by design. Each is `#[non_exhaustive]`, so promotions are non-breaking.

use crate::mrtr::ClientHandle;
use crate::progress::ProgressReporter;
use turbomcp4_core::RequestContext;

/// Define a per-RPC context that wraps only the shared [`RequestContext`].
macro_rules! plain_context {
    ($(#[$attr:meta])* $name:ident, $what:literal) => {
        #[doc = concat!("Context for `", $what, "`.")]
        ///
        /// Wraps the shared per-request metadata. (Not MRTR-capable, so no
        /// `ClientHandle` — by design, per SEP-2322.)
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

/// Define a per-RPC context for a work-doing method: the plain shape plus the
/// [`ClientHandle`] (MRTR-capable per SEP-2322) and the [`ProgressReporter`].
/// Handlers calling `ctx.client.elicit(…)` are re-executed from the top on
/// each round trip — see [`ClientHandle`].
macro_rules! mrtr_context {
    ($(#[$attr:meta])* $name:ident, $what:literal) => {
        #[doc = concat!("Context for `", $what, "` (MRTR-capable, SEP-2322).")]
        ///
        /// Wraps the shared per-request metadata plus the client-interaction
        /// handle and the request's progress reporter.
        $(#[$attr])*
        #[derive(Debug, Clone)]
        #[non_exhaustive]
        pub struct $name {
            /// Shared per-request metadata.
            pub base: RequestContext,
            /// The handler's channel to the client (elicitation, sampling,
            /// roots). MRTR on the draft; inline bidi on `2025-11-25`.
            pub client: ClientHandle,
            /// Progress reporting for this request; inert unless the request
            /// carried a `_meta.progressToken`.
            pub progress: ProgressReporter,
        }

        impl $name {
            /// Wrap a [`RequestContext`] (with no client channel or progress
            /// token attached — the dispatcher attaches them internally).
            #[must_use]
            pub fn new(base: RequestContext) -> Self {
                Self {
                    base,
                    client: ClientHandle::unavailable("no client channel attached"),
                    progress: ProgressReporter::disabled(),
                }
            }

            /// Attach the request's client-interaction handle.
            #[must_use]
            pub(crate) fn with_client(mut self, client: ClientHandle) -> Self {
                self.client = client;
                self
            }

            /// Attach the request's progress reporter.
            #[must_use]
            pub(crate) fn with_progress(mut self, progress: ProgressReporter) -> Self {
                self.progress = progress;
                self
            }
        }
    };
}

plain_context!(ListToolsContext, "tools/list");
mrtr_context!(CallToolContext, "tools/call");
plain_context!(ListResourcesContext, "resources/list");
plain_context!(ListResourceTemplatesContext, "resources/templates/list");
mrtr_context!(ReadResourceContext, "resources/read");
plain_context!(ListPromptsContext, "prompts/list");
mrtr_context!(GetPromptContext, "prompts/get");
plain_context!(CompleteContext, "completion/complete");
