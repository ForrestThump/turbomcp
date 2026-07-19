# turbomcp-auth

TurboMCP v4 auth: OAuth 2.1 resource-server validation (JWT/JWKS bearer tokens, RFC 8707 audience binding, RFC 9728 protected-resource metadata) and, behind the `oauth-client` feature, the OAuth 2.1 client flow (authorization-code + PKCE, discovery, dynamic registration, RFC 9207 issuer validation).

Part of [TurboMCP](https://github.com/Epistates/turbomcp), a Rust SDK for the
[Model Context Protocol](https://modelcontextprotocol.io). Most users should
depend on the [`turbomcp`](https://crates.io/crates/turbomcp) facade, which
re-exports this crate's surface behind one dependency and its feature flags.

## License

MIT
