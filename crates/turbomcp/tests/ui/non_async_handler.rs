//! Handler methods must be async.
use turbomcp::prelude::*;

#[derive(Clone)]
struct S;

#[server(name = "s", version = "1.0.0")]
impl S {
    #[tool]
    fn not_async(&self) -> String {
        "x".into()
    }
}

fn main() {}
