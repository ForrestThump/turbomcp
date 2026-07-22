//! A #[completion] handler takes exactly one CompleteParams argument.
use turbomcp::prelude::*;

#[derive(Clone)]
struct S;

#[server(name = "s", version = "1.0.0")]
impl S {
    #[completion]
    async fn complete(&self) -> McpResult<neutral::CompleteResult> {
        Ok(neutral::CompleteResult::new(vec![]))
    }
}

fn main() {}
