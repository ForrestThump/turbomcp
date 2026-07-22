//! Every resource handler argument must name a URI-template variable.
use turbomcp::prelude::*;

#[derive(Clone)]
struct S;

#[server(name = "s", version = "1.0.0")]
impl S {
    #[resource("file://{path}")]
    async fn file(&self, name: String) -> McpResult<String> {
        Ok(name)
    }
}

fn main() {}
