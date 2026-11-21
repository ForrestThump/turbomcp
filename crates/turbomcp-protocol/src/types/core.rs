//! Core protocol types and utilities
//!
//! This module contains the fundamental types used throughout the MCP protocol
//! implementation. These types are shared across multiple protocol features
//! and provide the foundational building blocks for the protocol.
//!
//! # Core Types
//!
//! - [`ProtocolVersion`] - Protocol version identifier
//! - [`RequestId`] - JSON-RPC request identifier
//! - [`BaseMetadata`] - Common name/title structure
//! - [`Implementation`] - Implementation information
//! - [`Annotations`] - Common annotation structure
//! - [`Role`] - Message role enum (User/Assistant)
//! - [`JsonRpcError`] - JSON-RPC error structure
//! - [`Timestamp`] - UTC timestamp wrapper

use crate::MessageId;
use jiff::{SignedDuration, Timestamp as JiffTimestamp};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Timestamp wrapper for consistent time handling
///
/// The inner value is a [`jiff::Timestamp`], an absolute point in time. It is
/// serialized as an RFC 3339 string in UTC (for example
/// `2026-11-20T22:15:03.123456Z`) using exactly the same byte layout that
/// `chrono::DateTime<Utc>` produced before the jiff migration, so existing
/// MCP peers keep seeing the timestamps they always saw.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub JiffTimestamp);

impl Timestamp {
    /// Create a new timestamp with current time
    #[must_use]
    pub fn now() -> Self {
        Self(JiffTimestamp::now())
    }

    /// Create a timestamp from an absolute instant
    #[must_use]
    pub const fn from_datetime(dt: JiffTimestamp) -> Self {
        Self(dt)
    }

    /// Get the inner instant
    #[must_use]
    pub const fn datetime(&self) -> JiffTimestamp {
        self.0
    }

    /// Get duration since this timestamp
    #[must_use]
    pub fn elapsed(&self) -> SignedDuration {
        JiffTimestamp::now().duration_since(self.0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", turbomcp_core::timestamp::format(&self.0))
    }
}

impl From<JiffTimestamp> for Timestamp {
    fn from(dt: JiffTimestamp) -> Self {
        Self(dt)
    }
}

impl Serialize for Timestamp {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        turbomcp_core::timestamp::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        turbomcp_core::timestamp::deserialize(deserializer).map(Self)
    }
}

/// Canonical MCP types re-exported from [`turbomcp_types`].
pub use turbomcp_types::{
    Annotations, Base64String, BaseMetadata, Cursor, Icon, IconTheme, Implementation, MimeType,
    ProtocolVersion, Role, Uri,
};

/// JSON-RPC request identifier
pub type RequestId = MessageId;

/// MCP progress notification token.
///
/// Per MCP 2025-11-25 (`schema.ts:23`), `ProgressToken = string | number`. It
/// commonly mirrors a `_meta.progressToken` value or the originating request's
/// numeric ID, so it shares its wire shape with [`RequestId`] / [`MessageId`].
pub type ProgressToken = MessageId;

// Re-export error codes from canonical location (crate::error_codes has more codes)
pub use crate::error_codes;

// Re-export JsonRpcError from canonical jsonrpc module
pub use crate::jsonrpc::JsonRpcError;

/// Base result type for MCP protocol responses (not `std::result::Result`).
///
/// This is the MCP protocol's `Result` structure, which carries optional
/// `_meta` metadata. It is distinct from Rust's `std::result::Result<T, E>`.
///
/// Per the current MCP specification, all result types should support
/// optional metadata in the `_meta` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Result {
    /// Optional metadata per the current MCP specification
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub _meta: Option<serde_json::Value>,
}

impl Result {
    /// Create a new result with no metadata
    #[must_use]
    pub fn new() -> Self {
        Self { _meta: None }
    }

    /// Create a result with metadata
    #[must_use]
    pub fn with_meta(meta: serde_json::Value) -> Self {
        Self { _meta: Some(meta) }
    }

    /// Add metadata to this result
    pub fn set_meta(&mut self, meta: serde_json::Value) {
        self._meta = Some(meta);
    }
}

impl Default for Result {
    fn default() -> Self {
        Self::new()
    }
}

/// A response that indicates success but carries no data
///
/// Per the current MCP specification, this is simply a Result with no additional fields.
/// This is used for operations where the success of the operation itself
/// is the only meaningful response, such as ping responses.
pub type EmptyResult = Result;

// `ModelHint`, `ModelPreferences` — canonical in `turbomcp_types::protocol`,
// re-exported at the module level via `pub use sampling::*` in `mod.rs`.
// `Icon`, `IconTheme` — canonical in `turbomcp_types::definitions`, re-exported
// above. See Decision 9 in the consolidation plan for the String-typed `src`
// rationale.
