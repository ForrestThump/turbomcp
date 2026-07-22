//! A #[server] may declare at most one #[completion] handler.
use turbomcp::prelude::*;

#[derive(Clone)]
struct S;

#[server(name = "s", version = "1.0.0")]
impl S {
    #[completion]
    async fn one(&self, _p: neutral::CompleteParams) -> McpResult<neutral::CompleteResult> {
        Ok(neutral::CompleteResult::new(vec![]))
    }

    #[completion]
    async fn two(&self, _p: neutral::CompleteParams) -> McpResult<neutral::CompleteResult> {
        Ok(neutral::CompleteResult::new(vec![]))
    }
}

fn main() {}
