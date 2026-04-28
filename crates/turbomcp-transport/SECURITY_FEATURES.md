# TurboMCP Transport Security Features

The transport crate owns transport-local security primitives: origin matching,
message-size validation, API-key validation helpers, session security metadata,
and reusable security configuration builders.

HTTP server security is implemented in `turbomcp-server` and higher-level
frontends such as `turbomcp-proxy`. OAuth, JWT, JWKS, and DPoP validation live
in `turbomcp-auth`.

## API-Key Validation

```rust
use turbomcp_transport::security::{AuthConfig, AuthMethod, validate_authentication};

let mut auth = AuthConfig::default();
auth.add_api_key("test_key_abcdefghijklmnopqrstuvwxyz123456".to_string());
auth.set_method(AuthMethod::Bearer);

let headers = [("authorization".to_string(), "Bearer test_key_abcdefghijklmnopqrstuvwxyz123456".to_string())]
    .into_iter()
    .collect();

validate_authentication(&auth, &headers)?;
# Ok::<(), turbomcp_transport::SecurityError>(())
```

## Origin Policy

```rust
use turbomcp_transport::security::OriginConfig;

let origins = OriginConfig {
    allowed_origins: vec!["https://app.example.com".to_string()],
    allow_localhost: false,
};
```

For Streamable HTTP servers, prefer `turbomcp_server::ServerConfig::builder()`
and its `allow_origin` / `allow_origins` methods.

## Message Limits

```rust
use turbomcp_transport::security::validate_message_size;

validate_message_size(1024, 10 * 1024 * 1024)?;
# Ok::<(), turbomcp_transport::SecurityError>(())
```

## Recommended Stack

- Use `turbomcp-server` for spec-compliant Streamable HTTP serving.
- Use `turbomcp-auth` for OAuth 2.1, JWT/JWKS validation, and DPoP.
- Use `turbomcp-transport` security helpers for transport-local API-key,
  origin, rate-limit, and session checks.
