//! `WWW-Authenticate` Bearer challenge parsing (RFC 6750 §3 / RFC 9728 §5.1).
//!
//! The MCP authorization spec requires clients to parse `WWW-Authenticate`
//! headers on `401`/`403` responses: the `resource_metadata` parameter locates
//! the Protected Resource Metadata document, `scope` is the authoritative
//! scope guidance for the current operation, and `error="insufficient_scope"`
//! marks a runtime step-up challenge.

use std::collections::BTreeMap;

/// A parsed `Bearer` challenge from a `WWW-Authenticate` header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BearerChallenge {
    /// `resource_metadata` — URL of the RFC 9728 document.
    pub resource_metadata: Option<String>,
    /// `scope` — space-delimited scopes required for the current operation
    /// (authoritative per the MCP scope-selection strategy).
    pub scope: Option<String>,
    /// `error` — e.g. `invalid_token`, `insufficient_scope`.
    pub error: Option<String>,
    /// `error_description` — human-readable detail.
    pub error_description: Option<String>,
}

impl BearerChallenge {
    /// Whether this is a runtime `insufficient_scope` (step-up) challenge.
    #[must_use]
    pub fn is_insufficient_scope(&self) -> bool {
        self.error.as_deref() == Some("insufficient_scope")
    }

    /// The challenged scopes as a list.
    #[must_use]
    pub fn scopes(&self) -> Vec<String> {
        self.scope
            .as_deref()
            .unwrap_or_default()
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect()
    }
}

/// Parse the `Bearer` challenge out of a `WWW-Authenticate` header value.
/// Returns `None` when no `Bearer` challenge is present.
///
/// Handles RFC 7235 auth-params: `token=value` and `token="quoted value"`
/// (with `\"` escapes), comma-separated, case-insensitive scheme and
/// parameter names. Unknown parameters are ignored.
#[must_use]
pub fn parse_bearer_challenge(header: &str) -> Option<BearerChallenge> {
    // Find the Bearer scheme (a header may carry multiple challenges; we take
    // the first Bearer one). The scheme is followed by whitespace or is the
    // whole (parameter-less) challenge.
    let lower = header.to_ascii_lowercase();
    let start = find_scheme(&lower, "bearer")?;
    let rest = &header[start + "bearer".len()..];

    let mut challenge = BearerChallenge::default();
    for (name, value) in parse_auth_params(rest) {
        match name.to_ascii_lowercase().as_str() {
            "resource_metadata" => challenge.resource_metadata = Some(value),
            "scope" => challenge.scope = Some(value),
            "error" => challenge.error = Some(value),
            "error_description" => challenge.error_description = Some(value),
            _ => {}
        }
    }
    Some(challenge)
}

/// Locate `scheme` as a standalone token in a lowercased header value.
fn find_scheme(lower: &str, scheme: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(pos) = lower[from..].find(scheme) {
        let at = from + pos;
        let before_ok = at == 0
            || lower[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c == ' ' || c == ',' || c == '\t');
        let after = lower[at + scheme.len()..].chars().next();
        let after_ok = after.is_none() || after.is_some_and(|c| c == ' ' || c == '\t');
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + scheme.len();
    }
    None
}

/// Parse `name=value` / `name="value"` pairs until the next challenge scheme
/// (or end of input). Stops at a bare token that isn't followed by `=` — that
/// is the next challenge's scheme name per RFC 7235's grammar.
fn parse_auth_params(input: &str) -> BTreeMap<String, String> {
    let mut params = BTreeMap::new();
    let mut rest = input;
    loop {
        rest = rest.trim_start_matches([' ', '\t', ',']);
        if rest.is_empty() {
            break;
        }
        // token
        let name_end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        if name.is_empty() {
            break;
        }
        let after = rest[name_end..].trim_start();
        let Some(after_eq) = after.strip_prefix('=') else {
            break; // next challenge scheme, not a parameter
        };
        let after_eq = after_eq.trim_start();
        if let Some(quoted) = after_eq.strip_prefix('"') {
            // quoted-string with backslash escapes
            let mut value = String::new();
            let mut chars = quoted.char_indices();
            let mut consumed = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, escaped)) = chars.next() {
                            value.push(escaped);
                        }
                    }
                    '"' => {
                        consumed = i + 1;
                        break;
                    }
                    other => value.push(other),
                }
            }
            params.insert(name.to_owned(), value);
            rest = &quoted[consumed..];
        } else {
            let value_end = after_eq.find([',', ' ', '\t']).unwrap_or(after_eq.len());
            params.insert(name.to_owned(), after_eq[..value_end].to_owned());
            rest = &after_eq[value_end..];
        }
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_spec_example() {
        let c = parse_bearer_challenge(
            "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource\", scope=\"files:read\"",
        )
        .unwrap();
        assert_eq!(
            c.resource_metadata.as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(c.scope.as_deref(), Some("files:read"));
        assert!(!c.is_insufficient_scope());
    }

    #[test]
    fn parses_insufficient_scope_step_up() {
        let c = parse_bearer_challenge(
            "Bearer error=\"insufficient_scope\", scope=\"files:write files:admin\", resource_metadata=\"https://m.example/.well-known/oauth-protected-resource\", error_description=\"File write permission required\"",
        )
        .unwrap();
        assert!(c.is_insufficient_scope());
        assert_eq!(c.scopes(), vec!["files:write", "files:admin"]);
        assert_eq!(
            c.error_description.as_deref(),
            Some("File write permission required")
        );
    }

    #[test]
    fn unquoted_params_and_case_insensitive_scheme() {
        let c = parse_bearer_challenge("bearer scope=files:read, error=invalid_token").unwrap();
        assert_eq!(c.scope.as_deref(), Some("files:read"));
        assert_eq!(c.error.as_deref(), Some("invalid_token"));
    }

    #[test]
    fn bare_bearer_and_absent_bearer() {
        assert_eq!(
            parse_bearer_challenge("Bearer"),
            Some(BearerChallenge::default())
        );
        assert!(parse_bearer_challenge("Basic realm=\"x\"").is_none());
    }

    #[test]
    fn multiple_challenges_take_bearer_params_only() {
        let c = parse_bearer_challenge("Basic realm=\"b\", Bearer scope=\"a b\"").unwrap();
        assert_eq!(c.scopes(), vec!["a", "b"]);
    }

    #[test]
    fn quoted_string_escapes() {
        let c = parse_bearer_challenge(r#"Bearer error_description="say \"hi\" now""#).unwrap();
        assert_eq!(c.error_description.as_deref(), Some(r#"say "hi" now"#));
    }
}
