//! # turbomcp4-protocol
//!
//! Per-spec-version typed MCP wire modules. Each version's `types` module is
//! **@generated** by `turbomcp4-codegen` from the official schema and checked
//! in (reviewed on each spec update); behaviour (dispatch, capability
//! negotiation) is hand-written.
//!
//! - [`v2025_11_25`] — stateful model: `initialize`, `ping`, `resources/subscribe`,
//!   and **core Tasks** (`tasks/get|list|cancel|result`).
//! - [`v2026_draft`] — `DRAFT-2026-v1`: stateless `server/discover`,
//!   `subscriptions/listen`, MRTR (`InputRequiredResult`); Tasks moves to the
//!   `extensions` mechanism.
//!
//! The cross-version `neutral` subset and `VersionDispatcher` land in Phase 2.
//!
//! `no_std + alloc`: the generated types use only `core`/`alloc` paths (the
//! codegen remaps typify's `::std::` output), so this crate is wasm-portable.
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use turbomcp4_core as _;

pub mod v2025_11_25;
pub mod v2026_draft;
