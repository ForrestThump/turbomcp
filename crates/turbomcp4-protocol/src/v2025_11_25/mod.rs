//! MCP `2025-11-25` — stateful protocol model.
//!
//! Wire string `"2025-11-25"`. Stateful: `initialize` handshake, `ping`,
//! `resources/subscribe`/`unsubscribe`, and **core Tasks**
//! (`tasks/get|list|cancel|result` + `notifications/tasks/status`). The
//! [`types`] module is `@generated` by `turbomcp4-codegen` — do not edit.

pub mod types;
