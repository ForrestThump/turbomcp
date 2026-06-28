//! [`ProtocolVersion`] — the single representation of an MCP protocol version.
//!
//! Ground truth (verified against `reference/modelcontextprotocol/schema/`):
//! the published versions are `2024-11-05`, `2025-03-26`, `2025-06-18`,
//! `2025-11-25`, and the in-development draft whose wire string is
//! **`DRAFT-2026-v1`** (`schema/draft/schema.ts:LATEST_PROTOCOL_VERSION`).
//!
//! The earlier plan hardcoded a fabricated `2026-07-28` date; that string
//! appears nowhere in the spec and would route every real draft client into
//! [`ProtocolVersion::Unknown`]. The variant is [`ProtocolVersion::Draft2026V1`]
//! and its wire value is provisional — bump it at spec freeze.

use alloc::string::{String, ToString};

/// An MCP protocol version.
///
/// `#[non_exhaustive]` so new versions can be added without a major bump.
/// Serializes to / deserializes from the wire string (e.g. `"2025-11-25"`,
/// `"DRAFT-2026-v1"`); unrecognized strings round-trip through
/// [`ProtocolVersion::Unknown`] rather than failing to parse.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(from = "String", into = "String")]
#[non_exhaustive]
pub enum ProtocolVersion {
    /// `2024-11-05` — first stable revision.
    V2024_11_05,
    /// `2025-03-26` — added (and is the only version to have) JSON-RPC batches.
    V2025_03_26,
    /// `2025-06-18` — removed batches.
    V2025_06_18,
    /// `2025-11-25` — current stable; stateful, core Tasks, `initialize`/`ping`.
    V2025_11_25,
    /// `DRAFT-2026-v1` — in-development stateless model (`server/discover`,
    /// `subscriptions/listen`, MRTR). **Provisional wire string.**
    Draft2026V1,
    /// Any version string this build does not recognize.
    Unknown(String),
}

impl ProtocolVersion {
    /// The latest version this build targets.
    pub const LATEST: Self = Self::Draft2026V1;

    /// Versions v4 actively supports as first-class (others may still be
    /// negotiated/named, but are not first-class dispatch targets).
    pub const SUPPORTED: &'static [Self] = &[Self::V2025_11_25, Self::Draft2026V1];

    /// The wire string for this version.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::V2024_11_05 => "2024-11-05",
            Self::V2025_03_26 => "2025-03-26",
            Self::V2025_06_18 => "2025-06-18",
            Self::V2025_11_25 => "2025-11-25",
            Self::Draft2026V1 => "DRAFT-2026-v1",
            Self::Unknown(s) => s,
        }
    }

    /// Parse a wire string into a [`ProtocolVersion`]. Unrecognized strings
    /// become [`ProtocolVersion::Unknown`] (never an error).
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            "2024-11-05" => Self::V2024_11_05,
            "2025-03-26" => Self::V2025_03_26,
            "2025-06-18" => Self::V2025_06_18,
            "2025-11-25" => Self::V2025_11_25,
            "DRAFT-2026-v1" => Self::Draft2026V1,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// Whether this build supports `self` as a first-class dispatch target.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        Self::SUPPORTED.contains(self)
    }
}

impl core::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for ProtocolVersion {
    /// Infallible: unrecognized strings map to [`ProtocolVersion::Unknown`]
    /// (reusing the owned `String`, no extra allocation).
    fn from(s: String) -> Self {
        match s.as_str() {
            "2024-11-05" => Self::V2024_11_05,
            "2025-03-26" => Self::V2025_03_26,
            "2025-06-18" => Self::V2025_06_18,
            "2025-11-25" => Self::V2025_11_25,
            "DRAFT-2026-v1" => Self::Draft2026V1,
            _ => Self::Unknown(s),
        }
    }
}

impl From<ProtocolVersion> for String {
    fn from(v: ProtocolVersion) -> Self {
        match v {
            ProtocolVersion::Unknown(s) => s,
            other => other.as_str().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn roundtrip_known_versions() {
        for v in [
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::Draft2026V1,
            ProtocolVersion::V2025_06_18,
        ] {
            let s = serde_json::to_string(&v).unwrap();
            let back: ProtocolVersion = serde_json::from_str(&s).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn draft_wire_string_is_correct() {
        assert_eq!(ProtocolVersion::Draft2026V1.as_str(), "DRAFT-2026-v1");
        assert_eq!(
            serde_json::to_string(&ProtocolVersion::Draft2026V1).unwrap(),
            "\"DRAFT-2026-v1\""
        );
    }

    #[test]
    fn unknown_roundtrips_not_errors() {
        let v: ProtocolVersion = serde_json::from_str("\"2099-01-01\"").unwrap();
        assert_eq!(v, ProtocolVersion::Unknown("2099-01-01".to_string()));
        assert_eq!(serde_json::to_string(&v).unwrap(), "\"2099-01-01\"");
    }

    #[test]
    fn supported_set() {
        assert!(ProtocolVersion::V2025_11_25.is_supported());
        assert!(ProtocolVersion::Draft2026V1.is_supported());
        assert!(!ProtocolVersion::V2024_11_05.is_supported());
    }
}
