//! A fixed-URI #[resource] cannot take handler arguments.
use turbomcp::prelude::*;

#[derive(Clone)]
struct S;

#[server(name = "s", version = "1.0.0")]
impl S {
    #[resource("config://app")]
    async fn config(&self, path: String) -> McpResult<String> {
        Ok(path)
    }
}

fn main() {}
