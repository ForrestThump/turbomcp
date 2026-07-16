//! Official MCP conformance suite, run against a TurboMCP server.
//!
//! This crate is intentionally empty at the library level — it exists to host
//! the conformance harness and the representative "everything"-style TurboMCP
//! server it drives. See `tests/conformance.rs`. It is `exclude`d from the
//! parent workspace so the Node harness dependency never touches the main
//! lockfile/gate; run it on its own:
//!
//! ```text
//! cd crates/turbomcp-conformance && cargo test
//! ```
