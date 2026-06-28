//! # Input validation (v4)
//!
//! Tools that validate their arguments in the handler body and reject bad input
//! with `McpError::invalid_params`. The macro already deserializes arguments
//! against the generated JSON schema (a type mismatch never reaches your code);
//! these checks are the *business* rules the schema can't express — ranges,
//! formats, cross-field constraints.
//!
//! A returned `McpError` becomes a `CallToolResult` with `isError: true` (a
//! tool-level error the model can see and react to), not a JSON-RPC transport
//! error.
//!
//! Run with: `cargo run -p turbomcp --example validation`

use turbomcp::prelude::*;

#[derive(Clone)]
struct ValidationServer;

#[server(name = "validation-demo", version = "1.0.0")]
impl ValidationServer {
    /// Create a user, enforcing a sensible age range.
    #[tool(description = "Create a user with age validation")]
    async fn create_user(&self, name: String, age: i64) -> McpResult<String> {
        if name.trim().is_empty() {
            return Err(McpError::invalid_params("name cannot be empty"));
        }
        if !(18..=120).contains(&age) {
            return Err(McpError::invalid_params("age must be between 18 and 120"));
        }
        Ok(format!("created user {name} (age {age})"))
    }

    /// Subscribe an email address after a basic format check.
    #[tool(description = "Subscribe with email validation")]
    async fn subscribe(&self, email: String) -> McpResult<String> {
        let (local, domain) = email
            .split_once('@')
            .ok_or_else(|| McpError::invalid_params("email must contain exactly one '@'"))?;
        if local.is_empty() || domain.contains('@') {
            return Err(McpError::invalid_params(
                "email must contain exactly one '@'",
            ));
        }
        if !domain.contains('.') {
            return Err(McpError::invalid_params("email domain must contain a '.'"));
        }
        Ok(format!("subscribed {email}"))
    }

    /// Set a temperature constrained to the inclusive range `0.0..=1.0`.
    #[tool(description = "Set temperature (0.0-1.0)")]
    async fn set_temperature(&self, temp: f64) -> McpResult<String> {
        if !(0.0..=1.0).contains(&temp) {
            return Err(McpError::invalid_params(
                "temperature must be between 0.0 and 1.0",
            ));
        }
        Ok(format!("temperature set to {temp:.2}"))
    }

    /// Create a username with length and character-set rules.
    #[tool(description = "Create a username (3-20 chars, alphanumeric or _)")]
    async fn create_username(&self, username: String) -> McpResult<String> {
        if !(3..=20).contains(&username.len()) {
            return Err(McpError::invalid_params("username must be 3-20 characters"));
        }
        if !username.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(McpError::invalid_params(
                "username may only contain letters, numbers, and underscores",
            ));
        }
        Ok(format!("username created: {username}"))
    }
}

#[tokio::main]
async fn main() -> Result<(), turbomcp::ProtocolError> {
    // Logs MUST go to stderr — stdout carries the MCP protocol framing.
    ValidationServer.run_stdio().await
}
