//! # Dual-transport server (v4)
//!
//! The Phase 4 exit criterion in example form: one `#[server]` type served over
//! **either** stdio or Streamable HTTP, chosen at runtime, with no change to the
//! server itself.
//!
//! ```text
//! cargo run -p turbomcp4 --example dual_transport --features http              # stdio (default)
//! cargo run -p turbomcp4 --example dual_transport --features http -- http      # http on 127.0.0.1:8080
//! cargo run -p turbomcp4 --example dual_transport --features http -- http 0.0.0.0:9000
//! ```

use std::net::SocketAddr;

use turbomcp4::http::{HttpConfig, serve_http};
use turbomcp4::prelude::*;

#[derive(Clone)]
struct Calc;

#[server(name = "calc", version = "1.0.0")]
impl Calc {
    /// Add two numbers.
    #[tool(description = "Add two numbers")]
    async fn add(&self, a: f64, b: f64) -> McpResult<String> {
        Ok((a + b).to_string())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Logs MUST go to stderr — on stdio, stdout carries the MCP protocol framing.
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("http") => {
            let addr: SocketAddr = args
                .next()
                .unwrap_or_else(|| "127.0.0.1:8080".to_owned())
                .parse()?;
            eprintln!("serving calc over HTTP on http://{addr}/mcp");
            // `.into_server()` resolves to the macro's inherent method, so the
            // tool capability is pre-registered before we build the dispatcher.
            let service = Calc.into_server().build();
            serve_http(addr, service, HttpConfig::new()).await?;
        }
        _ => {
            eprintln!("serving calc over stdio");
            Calc.run_stdio().await?;
        }
    }
    Ok(())
}
