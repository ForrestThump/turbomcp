//! Path validation for security
//!
//! This module provides focused path validation utilities to prevent common
//! security vulnerabilities like path traversal attacks. It follows the principle
//! of doing one thing well rather than trying to cover every possible security scenario.

use crate::Result;
use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Decode URL-encoded patterns until the result is a fixed point.
///
/// Catches arbitrarily-deep nested encoding (`%252e` → `%2e` → `.`,
/// `%25252e` → `%252e` → `%2e` → `.`) up to a small bounded number of
/// iterations to avoid pathological inputs that flip-flop. In practice
/// 8 passes is sufficient for any realistic attacker.
fn decode_url_encoded(s: &str) -> String {
    const MAX_PASSES: usize = 8;
    let mut current = s.to_string();
    for _ in 0..MAX_PASSES {
        let next = percent_decode_str(&current).decode_utf8_lossy().to_string();
        if next == current {
            return next;
        }
        current = next;
    }
    current
}

/// Check for path traversal patterns including Unicode lookalikes
/// v2.3.6: Added for enhanced path traversal detection
fn contains_traversal_pattern(s: &str) -> bool {
    // Standard traversal
    if s.contains("..") {
        return true;
    }
    // Unicode lookalikes for dots (fullwidth, ideographic)
    if s.contains("．．") || s.contains("。。") {
        return true;
    }
    // Backslash variants
    if s.contains("..\\") || s.contains("\\..") {
        return true;
    }
    false
}

/// Validates a path for basic security constraints
///
/// This function performs essential security checks:
/// - Canonicalizes the path to resolve symlinks and relative components
/// - Prevents path traversal attacks by checking for ".." patterns
/// - Validates that the path is within reasonable bounds
///
/// # Examples
///
/// ```rust,no_run
/// use turbomcp_protocol::security::validate_path;
///
/// // Safe path
/// let safe_path = validate_path("/home/user/data.txt")?;
///
/// // Path traversal attempt - will fail
/// let result = validate_path("/home/user/../../../etc/passwd");
/// assert!(result.is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
/// Run only the textual / lexical checks on a path:
/// null-byte detection, URL-encoded traversal patterns, Unicode
/// lookalikes. Does **not** touch the filesystem, so it is safe to call
/// before a file is created or for paths that don't exist yet.
///
/// Use this for "validate before write" flows; use [`validate_path`] when
/// you need the canonicalized real path on disk.
pub fn validate_path_syntactic<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    let path_str = path.to_string_lossy();

    if path_str.contains('\0') || path_str.contains("%00") {
        return Err(crate::Error::security(format!(
            "Null byte in path detected: {path:?}"
        )));
    }

    let decoded = decode_url_encoded(&path_str);
    if contains_traversal_pattern(&path_str) || contains_traversal_pattern(&decoded) {
        return Err(crate::Error::security(format!(
            "Path traversal pattern detected: {path:?}"
        )));
    }
    Ok(())
}

/// Validate a path that **must already exist on the filesystem**: runs
/// the lexical checks of [`validate_path_syntactic`] and then canonicalizes
/// to resolve symlinks. For paths that don't exist yet (write-target
/// validation), use [`validate_path_syntactic`] directly.
pub fn validate_path<P: AsRef<Path>>(path: P) -> Result<PathBuf> {
    let path = path.as_ref();
    debug!("Validating path: {:?}", path);

    // Lexical checks first — fast-fail without touching the filesystem.
    validate_path_syntactic(path)?;

    // Canonicalize the path to resolve symlinks and relative components
    let canonical_path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            warn!("Failed to canonicalize path {:?}: {}", path, e);
            return Err(crate::Error::security(format!(
                "Invalid path or access denied: {:?}",
                path
            )));
        }
    };

    // Basic sanity check on path depth to prevent excessive nesting
    let depth = canonical_path.components().count();
    if depth > 20 {
        // Reasonable limit for most use cases
        return Err(crate::Error::security(format!(
            "Path depth too deep ({}): {:?}",
            depth, canonical_path
        )));
    }

    debug!("Path validation successful: {:?}", canonical_path);
    Ok(canonical_path)
}

/// Validates a path and enforces it's within a base directory
///
/// This is useful for ensuring file operations stay within allowed boundaries.
///
/// # Examples
///
/// ```rust,no_run
/// use turbomcp_protocol::security::validate_path_within;
///
/// let base = "/home/user/workspace";
/// let file_path = validate_path_within("/home/user/workspace/project/file.txt", base)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn validate_path_within<P: AsRef<Path>, B: AsRef<Path>>(path: P, base: B) -> Result<PathBuf> {
    let validated_path = validate_path(path)?;
    let base_path = base
        .as_ref()
        .canonicalize()
        .map_err(|e| crate::Error::security(format!("Invalid base path: {}", e)))?;

    if !validated_path.starts_with(&base_path) {
        return Err(crate::Error::security(format!(
            "Path outside allowed directory: {:?} not within {:?}",
            validated_path, base_path
        )));
    }

    Ok(validated_path)
}

/// Checks if a file extension is allowed
///
/// Simple utility for validating file extensions against an allow list.
/// Comparison is case-insensitive — `"PDF"` and `"pdf"` are treated as equal.
/// `allowed_extensions` should be supplied lowercase; non-lowercase entries
/// will still match because the input extension is lowercased before lookup.
pub fn validate_file_extension<P: AsRef<Path>>(path: P, allowed_extensions: &[&str]) -> Result<()> {
    let path = path.as_ref();

    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => {
            let ext_lower = ext.to_ascii_lowercase();
            if allowed_extensions
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(&ext_lower))
            {
                Ok(())
            } else {
                Err(crate::Error::security(format!(
                    "File extension '{}' not allowed",
                    ext
                )))
            }
        }
        None => {
            if allowed_extensions.is_empty() {
                Ok(()) // No extension required
            } else {
                Err(crate::Error::security(
                    "File must have an extension".to_string(),
                ))
            }
        }
    }
}
