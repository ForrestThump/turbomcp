//! MCP `2026-07-28` — the in-development draft's stateless protocol model.
//!
//! Wire string `"2026-07-28"` (the draft's `LATEST_PROTOCOL_VERSION`). The
//! string is final; the schema *content* still tracks `schema/draft/` and may
//! shift until the dated directory freezes (~2026-07-28).
//! Stateless: per-request `_meta` version, `server/discover`,
//! `subscriptions/listen`, MRTR (`InputRequiredResult`). Tasks is delivered via
//! the `extensions` capability rather than core methods. The [`types`] module
//! is `@generated` by `turbomcp-codegen` — do not edit.

pub mod types;
