//! Bucket-A A6 (part 2): `#[tool(scopes(…))]` denies a call unless the caller's
//! identity holds every required OAuth scope.

use serde_json::{Map, json};
use turbomcp::neutral::{CallToolParams, CallToolResult};
use turbomcp::prelude::*;
use turbomcp::{CallToolContext, Claims, Identity, ProtocolVersion, RequestContext, WithTools};

#[derive(Clone)]
struct Guarded;

#[server(name = "guarded", version = "1.0.0")]
impl Guarded {
    /// Requires the `admin` scope.
    #[tool(description = "Admin only", scopes("admin"))]
    async fn secret(&self) -> String {
        "top secret".into()
    }

    /// No scope requirement.
    #[tool(description = "Public")]
    async fn open(&self) -> String {
        "public".into()
    }
}

fn ctx(scope: Option<&str>) -> CallToolContext {
    let identity = match scope {
        Some(s) => {
            let mut claims = Claims::new();
            claims.insert("scope".into(), json!(s));
            Identity::Bearer {
                sub: "u".into(),
                claims,
            }
        }
        None => Identity::Anonymous,
    };
    CallToolContext::new(RequestContext::new(ProtocolVersion::LATEST).with_identity(identity))
}

async fn call(name: &str, ctx: &CallToolContext) -> CallToolResult {
    Guarded
        .call_tool(ctx, CallToolParams::new(name, Map::new()))
        .await
        .expect("call")
}

fn text(r: &CallToolResult) -> String {
    match &r.content[0] {
        turbomcp::neutral::Content::Text { text, .. } => text.clone(),
        other => panic!("expected text, got {other:?}"),
    }
}

#[tokio::test]
async fn scoped_tool_allows_caller_with_scope() {
    let r = call("secret", &ctx(Some("read admin write"))).await;
    assert!(!r.is_error, "should allow: {r:?}");
    assert_eq!(text(&r), "top secret");
}

#[tokio::test]
async fn scoped_tool_denies_caller_without_scope() {
    let r = call("secret", &ctx(Some("read write"))).await;
    assert!(r.is_error, "should deny");
    assert!(text(&r).contains("insufficient scope"), "got {}", text(&r));
}

#[tokio::test]
async fn scoped_tool_denies_anonymous() {
    let r = call("secret", &ctx(None)).await;
    assert!(r.is_error);
}

#[tokio::test]
async fn unscoped_tool_allows_anyone() {
    let r = call("open", &ctx(None)).await;
    assert!(!r.is_error);
    assert_eq!(text(&r), "public");
}
