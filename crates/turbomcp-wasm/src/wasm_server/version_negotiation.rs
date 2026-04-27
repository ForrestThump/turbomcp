//! MCP protocol version negotiation for WASM handlers.
//!
//! All three WASM handlers (`handler`, `streamable`, `middleware`) share the
//! same negotiation policy: accept the client's requested version if it is in
//! `ProtocolVersion::STABLE`; reject with a JSON-RPC `-32602` error otherwise.

use turbomcp_core::PROTOCOL_VERSION;
use turbomcp_types::ProtocolVersion;

/// The InitializeParams shape we care about — only the protocol version.
/// All three handlers have their own InitializeParams; this trait gives us a
/// single accessor.
pub(super) trait HasProtocolVersion {
    fn protocol_version_str(&self) -> &str;
}

/// Negotiate the protocol version against `ProtocolVersion::STABLE`.
///
/// Returns the negotiated `ProtocolVersion` to echo in `InitializeResult`.
/// On mismatch, returns `Err(supported_list_string)` for use in the JSON-RPC
/// error message.
pub(super) fn negotiate(
    params: Option<&impl HasProtocolVersion>,
) -> Result<ProtocolVersion, String> {
    let requested = match params {
        Some(p) if !p.protocol_version_str().is_empty() => p.protocol_version_str(),
        _ => {
            // Older clients may omit the version. Echo our latest as a graceful
            // default — same behavior as the `turbomcp-server` HTTP transport.
            return Ok(ProtocolVersion::from(PROTOCOL_VERSION));
        }
    };

    for v in ProtocolVersion::STABLE {
        if v.as_str() == requested {
            return Ok(v.clone());
        }
    }

    let supported = ProtocolVersion::STABLE
        .iter()
        .map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(supported)
}

// Implement HasProtocolVersion for each handler's InitializeParams via a small
// glue layer. Handlers create their own InitializeParams structs; we expose a
// single function `negotiate_str` they can call with the raw version string.

/// Convenience wrapper over a raw version string for callers who already
/// extracted it from their own params struct.
pub(super) fn negotiate_str(requested: Option<&str>) -> Result<ProtocolVersion, String> {
    struct Wrapper<'a>(&'a str);
    impl<'a> HasProtocolVersion for Wrapper<'a> {
        fn protocol_version_str(&self) -> &str {
            self.0
        }
    }
    let wrapped = requested.map(Wrapper);
    negotiate(wrapped.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_stable_versions() {
        for v in ProtocolVersion::STABLE {
            assert_eq!(
                negotiate_str(Some(v.as_str())).unwrap().as_str(),
                v.as_str()
            );
        }
    }

    #[test]
    fn rejects_unknown_versions() {
        assert!(negotiate_str(Some("1999-01-01")).is_err());
    }

    #[test]
    fn empty_or_missing_defaults_to_latest() {
        assert_eq!(negotiate_str(None).unwrap().as_str(), PROTOCOL_VERSION);
        assert_eq!(negotiate_str(Some("")).unwrap().as_str(), PROTOCOL_VERSION);
    }
}
