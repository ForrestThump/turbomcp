//! TurboMCP v4 wire codec: bytes <-> JsonRpcMessage (serde_json / sonic-rs / msgpack).
//!
//! TurboMCP v4 — Phase 0 skeleton. Implementation lands in later phases.
#![forbid(unsafe_code)]

// Phase 0: establish + verify the dependency edge on the foundation crate.
use turbomcp4_core as _;
