//! Authentication framework for transport layer
//!
//! This module provides flexible authentication mechanisms including
//! Bearer tokens, API keys, and custom header authentication.
//! Supports multiple authentication methods and environments.
//!
//! ## Security
//!
//! Token validation uses constant-time comparison via [`subtle::ConstantTimeEq`]
//! to prevent timing side-channels. The total validation time is `O(n)` in the
//! number of configured keys (each compared in constant time). For deployments
//! with thousands of keys, prefer pre-hashed lookup with a constant-time HMAC
//! verifier in front of this module.

use super::errors::SecurityError;
use crate::security::SecurityHeaders;
use std::collections::HashSet;
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

/// Compare a presented credential against the configured set in constant time
/// per candidate. The early-return on a match leaks only "matched / not
/// matched", not which key matched or where the bytes diverge.
fn ct_contains(set: &HashSet<String>, presented: &str) -> bool {
    let presented_bytes = presented.as_bytes();
    let mut matched = 0u8;
    for candidate in set {
        let eq: u8 = presented_bytes.ct_eq(candidate.as_bytes()).unwrap_u8();
        matched |= eq;
    }
    matched != 0
}

/// Authentication methods
#[derive(Clone, Debug)]
pub enum AuthMethod {
    /// Bearer token authentication
    Bearer,
    /// API key in Authorization header
    ApiKey,
    /// Custom header authentication
    Custom(String),
}

/// Authentication configuration.
///
/// API keys are stored as plain `String`s in a `HashSet` for fast set
/// operations and hash-based lookup; comparison itself goes through the
/// constant-time `ct_contains` helper to prevent timing leaks. On
/// `Drop`, every key is wiped via [`zeroize::Zeroize`] before the
/// allocation is released, mitigating recovery from process core dumps
/// or memory scanning.
#[derive(Clone, Debug)]
pub struct AuthConfig {
    /// Whether authentication is required
    pub require_auth: bool,
    /// Valid API keys for authentication
    pub api_keys: HashSet<String>,
    /// Authentication method
    pub method: AuthMethod,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            require_auth: false,
            api_keys: HashSet::new(),
            method: AuthMethod::Bearer,
        }
    }
}

impl Drop for AuthConfig {
    fn drop(&mut self) {
        // Zero each API key on drop. `HashSet` doesn't expose mutable access
        // to its values (so the contained `String`s cannot be `zeroize()`'d
        // in place), so we drain the set into temporary `String`s and zero
        // those — `String::zeroize` overwrites the heap buffer before drop.
        for mut k in self.api_keys.drain() {
            k.zeroize();
        }
    }
}

impl AuthConfig {
    /// Create a new authentication configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an API key
    pub fn add_api_key(&mut self, key: String) {
        self.api_keys.insert(key);
    }

    /// Add multiple API keys
    pub fn add_api_keys(&mut self, keys: Vec<String>) {
        self.api_keys.extend(keys);
    }

    /// Set whether authentication is required
    pub fn set_require_auth(&mut self, require: bool) {
        self.require_auth = require;
    }

    /// Set the authentication method
    pub fn set_method(&mut self, method: AuthMethod) {
        self.method = method;
    }

    /// Check if API key is valid (constant-time per candidate).
    ///
    /// Comparison is timing-safe: the time taken does not reveal which
    /// candidate key matched or where a non-matching key diverges. The total
    /// time is linear in the number of configured keys.
    pub fn is_valid_key(&self, key: &str) -> bool {
        ct_contains(&self.api_keys, key)
    }
}

/// Validate authentication credentials
pub fn validate_authentication(
    config: &AuthConfig,
    headers: &SecurityHeaders,
) -> Result<(), SecurityError> {
    if !config.require_auth {
        return Ok(());
    }

    match config.method {
        AuthMethod::Bearer => {
            let auth_header = headers.get("Authorization").ok_or_else(|| {
                SecurityError::AuthenticationFailed("Missing Authorization header".to_string())
            })?;
            validate_bearer_token(config, auth_header)
        }
        AuthMethod::ApiKey => {
            let auth_header = headers.get("Authorization").ok_or_else(|| {
                SecurityError::AuthenticationFailed("Missing Authorization header".to_string())
            })?;
            validate_api_key(config, auth_header)
        }
        AuthMethod::Custom(ref header_name) => validate_custom_header(config, headers, header_name),
    }
}

/// Validate Bearer token authentication
fn validate_bearer_token(config: &AuthConfig, auth_header: &str) -> Result<(), SecurityError> {
    if !auth_header.starts_with("Bearer ") {
        return Err(SecurityError::AuthenticationFailed(
            "Invalid Authorization format, expected Bearer token".to_string(),
        ));
    }

    let token = &auth_header[7..];
    if !ct_contains(&config.api_keys, token) {
        return Err(SecurityError::AuthenticationFailed(
            "Invalid bearer token".to_string(),
        ));
    }

    Ok(())
}

/// Validate API key authentication
fn validate_api_key(config: &AuthConfig, auth_header: &str) -> Result<(), SecurityError> {
    if !auth_header.starts_with("ApiKey ") {
        return Err(SecurityError::AuthenticationFailed(
            "Invalid Authorization format, expected ApiKey".to_string(),
        ));
    }

    let key = &auth_header[7..];
    if !ct_contains(&config.api_keys, key) {
        return Err(SecurityError::AuthenticationFailed(
            "Invalid API key".to_string(),
        ));
    }

    Ok(())
}

/// Validate custom header authentication.
///
/// Note: the error message intentionally does not echo the header *value*; it
/// only names the header. We do echo `header_name` because it is a static
/// configuration value, not peer-controlled input.
fn validate_custom_header(
    config: &AuthConfig,
    headers: &SecurityHeaders,
    header_name: &str,
) -> Result<(), SecurityError> {
    let custom_value = headers.get(header_name).ok_or_else(|| {
        SecurityError::AuthenticationFailed(format!("Missing {header_name} header"))
    })?;

    if !ct_contains(&config.api_keys, custom_value) {
        return Err(SecurityError::AuthenticationFailed(format!(
            "Invalid {header_name} value"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_auth_config_default() {
        let config = AuthConfig::default();
        assert!(!config.require_auth);
        assert!(config.api_keys.is_empty());
        assert!(matches!(config.method, AuthMethod::Bearer));
    }

    #[test]
    fn test_bearer_authentication_success() {
        let config = AuthConfig {
            require_auth: true,
            api_keys: vec!["secret123".to_string()].into_iter().collect(),
            method: AuthMethod::Bearer,
        };

        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer secret123".to_string());

        assert!(validate_authentication(&config, &headers).is_ok());
    }

    #[test]
    fn test_bearer_authentication_invalid_token() {
        let config = AuthConfig {
            require_auth: true,
            api_keys: vec!["secret123".to_string()].into_iter().collect(),
            method: AuthMethod::Bearer,
        };

        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer wrong".to_string());

        assert!(validate_authentication(&config, &headers).is_err());
    }

    #[test]
    fn test_bearer_authentication_invalid_format() {
        let config = AuthConfig {
            require_auth: true,
            api_keys: vec!["secret123".to_string()].into_iter().collect(),
            method: AuthMethod::Bearer,
        };

        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Basic secret123".to_string());

        assert!(validate_authentication(&config, &headers).is_err());
    }

    #[test]
    fn test_api_key_authentication() {
        let config = AuthConfig {
            require_auth: true,
            api_keys: vec!["api123".to_string()].into_iter().collect(),
            method: AuthMethod::ApiKey,
        };

        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "ApiKey api123".to_string());

        assert!(validate_authentication(&config, &headers).is_ok());
    }

    #[test]
    fn test_custom_header_authentication() {
        let config = AuthConfig {
            require_auth: true,
            api_keys: vec!["custom123".to_string()].into_iter().collect(),
            method: AuthMethod::Custom("X-API-Key".to_string()),
        };

        let mut headers = HashMap::new();
        headers.insert("X-API-Key".to_string(), "custom123".to_string());

        assert!(validate_authentication(&config, &headers).is_ok());
    }

    #[test]
    fn test_no_auth_required() {
        // Build the config field-by-field rather than using struct update
        // syntax: `AuthConfig` implements `Drop` (to zeroize API keys), and
        // E0509 forbids moving fields out of a Drop-impl'd value.
        let mut config = AuthConfig::default();
        config.require_auth = false;
        let headers = HashMap::new();

        assert!(validate_authentication(&config, &headers).is_ok());
    }

    #[test]
    fn test_missing_authorization_header() {
        let config = AuthConfig {
            require_auth: true,
            api_keys: vec!["secret".to_string()].into_iter().collect(),
            method: AuthMethod::Bearer,
        };
        let headers = HashMap::new();

        assert!(validate_authentication(&config, &headers).is_err());
    }
}
