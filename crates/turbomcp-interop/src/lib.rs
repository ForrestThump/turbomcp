//! Cross-SDK interop suite (tests-only). See `tests/rmcp_interop.rs`.
//!
//! This crate is intentionally empty at the library level — it exists to host
//! interop integration tests that link both `turbomcp` and the official `rmcp`
//! SDK. It is `exclude`d from the parent workspace so rmcp's dependency tree
//! stays out of the main lockfile/gate.
