//! TurboMCP v4 protocol crate — Phase 0 skeleton.
//!
//! Per-spec-version typed wire modules (`v2025_11_25`, `v2026_draft`) and the
//! cross-version `neutral` subset land in Phase 1 (codegen). `VersionDispatcher`
//! lands in Phase 2.
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
extern crate alloc;

use turbomcp4_core as _;
