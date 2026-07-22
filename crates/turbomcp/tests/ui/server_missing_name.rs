//! #[server] requires a `name`.
use turbomcp::prelude::*;

#[derive(Clone)]
struct S;

#[server(version = "1.0.0")]
impl S {
    #[tool]
    async fn greet(&self) -> String {
        "hi".into()
    }
}

fn main() {}
