//! MCP `DRAFT-2026-v1` — stateless protocol model.
//!
//! Wire string `"DRAFT-2026-v1"` (provisional; changes at spec freeze).
//! Stateless: per-request `_meta` version, `server/discover`,
//! `subscriptions/listen`, MRTR (`InputRequiredResult`). Tasks is delivered via
//! the `extensions` capability rather than core methods. The [`types`] module
//! is `@generated` by `turbomcp4-codegen` — do not edit.

pub mod types;
