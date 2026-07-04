//! [`Identity`] — who made the request.
//!
//! An enum (not an opaque `Box<dyn Any>`) so the common cases are visible to
//! handlers and rate-limit key extractors, with a [`Identity::Custom`] escape
//! hatch for patterns that don't fit (OAuth introspection, etc.).
//!
//! **Redaction (round-3 SC-9):** the [`core::fmt::Debug`] impl never prints
//! claim *values* — only the subject and the set of claim keys — so identities
//! flowing into `tracing` spans don't leak PII (emails, org ids) into telemetry
//! by default.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use serde_json::{Map, Value};

/// A bag of validated identity claims (e.g. decoded JWT/PASETO body).
pub type Claims = Map<String, Value>;

/// Escape hatch for identity shapes that don't fit the common variants.
///
/// Implementors are object-safe; the framework holds them behind `Arc`.
pub trait IdentityClaims: fmt::Debug + Send + Sync {
    /// The authenticated subject, if any.
    fn subject(&self) -> Option<&str>;
    /// Look up a claim by key.
    fn claim(&self, key: &str) -> Option<&Value>;
}

/// The identity associated with a request.
#[derive(Clone, Default)]
#[non_exhaustive]
pub enum Identity {
    /// No authentication was declared.
    #[default]
    Anonymous,
    /// Bearer-token identity (PASETO/JWT).
    Bearer {
        /// Authenticated subject.
        sub: String,
        /// Decoded claims.
        claims: Claims,
    },
    /// DPoP-bound identity (RFC 9449).
    Dpop {
        /// Authenticated subject.
        sub: String,
        /// JWK thumbprint (`jkt`) the token is bound to.
        jkt: String,
        /// Decoded claims.
        claims: Claims,
    },
    /// Caller-defined identity.
    Custom(Arc<dyn IdentityClaims>),
}

impl Identity {
    /// The authenticated subject, if this identity has one.
    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        match self {
            Self::Anonymous => None,
            Self::Bearer { sub, .. } | Self::Dpop { sub, .. } => Some(sub),
            Self::Custom(c) => c.subject(),
        }
    }

    /// Whether this identity is authenticated (anything other than anonymous).
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        !matches!(self, Self::Anonymous)
    }

    /// The claim *keys* present (never the values) — the redaction-safe view of
    /// the claim set, for logging/telemetry. `Custom` identities expose none
    /// (the trait yields values by key, not an enumerable key set).
    #[must_use]
    pub fn claim_keys(&self) -> Vec<&str> {
        match self {
            Self::Anonymous | Self::Custom(_) => Vec::new(),
            Self::Bearer { claims, .. } | Self::Dpop { claims, .. } => Self::claim_keys_of(claims),
        }
    }

    /// The claim keys of a specific claim set (values intentionally not exposed;
    /// use the typed variant fields for value access).
    fn claim_keys_of(claims: &Claims) -> Vec<&str> {
        claims.keys().map(String::as_str).collect()
    }

    /// Look up a claim *value* by key (for programmatic access such as scope
    /// checks). This does not affect the redaction-safe logging view — `Debug`,
    /// [`claim_keys`](Self::claim_keys), and [`RedactedSubject`] still never
    /// print claim values.
    #[must_use]
    pub fn claim(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Anonymous => None,
            Self::Bearer { claims, .. } | Self::Dpop { claims, .. } => claims.get(key),
            Self::Custom(c) => c.claim(key),
        }
    }

    /// The OAuth scopes granted to this identity, read from the standard `scope`
    /// (space-delimited string) and `scp` (array) claims.
    #[must_use]
    pub fn granted_scopes(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(Value::String(s)) = self.claim("scope") {
            out.extend(s.split_whitespace().map(String::from));
        }
        if let Some(Value::Array(arr)) = self.claim("scp") {
            out.extend(arr.iter().filter_map(|v| v.as_str().map(String::from)));
        }
        out
    }

    /// Whether this identity holds every scope in `required` (`true` if the
    /// requirement is empty). Anonymous identities hold no scopes.
    #[must_use]
    pub fn has_scopes(&self, required: &[&str]) -> bool {
        if required.is_empty() {
            return true;
        }
        let granted = self.granted_scopes();
        required.iter().all(|r| granted.iter().any(|g| g == r))
    }
}

impl fmt::Debug for Identity {
    /// Redaction-aware: prints subject + claim *keys*, never claim values.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Anonymous => f.write_str("Identity::Anonymous"),
            Self::Bearer { sub, claims } => f
                .debug_struct("Identity::Bearer")
                .field("sub", sub)
                .field("claim_keys", &Self::claim_keys_of(claims))
                .finish(),
            Self::Dpop { sub, jkt, claims } => f
                .debug_struct("Identity::Dpop")
                .field("sub", sub)
                .field("jkt", jkt)
                .field("claim_keys", &Self::claim_keys_of(claims))
                .finish(),
            Self::Custom(c) => f
                .debug_struct("Identity::Custom")
                .field("sub", &c.subject())
                .finish(),
        }
    }
}

/// A wrapper that hashes the subject for use in spans/logs where even the
/// subject is sensitive (round-3 SC-9 default). The framework's tracing layer
/// uses this rather than the raw subject.
#[derive(Clone, Copy)]
pub struct RedactedSubject<'a>(pub &'a Identity);

impl fmt::Display for RedactedSubject<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.subject() {
            None => f.write_str("anonymous"),
            // FNV-1a over the subject: stable, non-reversible-enough for log
            // correlation without exposing the raw value. Not a security
            // primitive — just keeps PII out of telemetry by default.
            Some(sub) => {
                let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
                for b in sub.as_bytes() {
                    hash ^= u64::from(*b);
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
                write!(f, "sub:{hash:016x}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use serde_json::json;

    #[test]
    fn debug_does_not_leak_claim_values() {
        let mut claims = Claims::new();
        claims.insert("email".into(), json!("secret@example.com"));
        let id = Identity::Bearer {
            sub: "user-1".into(),
            claims,
        };
        let dbg = format!("{id:?}");
        assert!(dbg.contains("user-1"));
        assert!(dbg.contains("email")); // key shown
        assert!(!dbg.contains("secret@example.com")); // value hidden
    }

    #[test]
    fn redacted_subject_is_stable_and_opaque() {
        let id = Identity::Bearer {
            sub: "alice".into(),
            claims: Claims::new(),
        };
        let a = format!("{}", RedactedSubject(&id));
        let b = format!("{}", RedactedSubject(&id));
        assert_eq!(a, b);
        assert!(!a.contains("alice"));
        assert_eq!(
            format!("{}", RedactedSubject(&Identity::Anonymous)),
            "anonymous"
        );
    }
}
