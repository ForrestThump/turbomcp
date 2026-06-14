//! TurboMCP v4 client.
//!
//! The other half of the protocol: a [`Client`] drives an MCP server over any
//! [`Transport`](turbomcp4_service::Transport). Phase 8a establishes the
//! connection actor and raw request/response plumbing; the typed MCP API
//! (`initialize`, `list_tools`, the `ConnectMode` probe, the MRTR client loop)
//! builds on top of it in later sub-phases.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod connection;
mod error;

pub use connection::{Client, DEFAULT_REQUEST_TIMEOUT};
pub use error::{ClientError, ClientResult};
