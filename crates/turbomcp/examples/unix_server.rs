//! Unix Socket Transport Server - Minimal Example
//!
//! Demonstrates local IPC over a Unix domain socket.
//!
//! **Run:**
//! ```bash
//! cargo run --example unix_server --features unix
//! ```
//!
//! **Connect:**
//! ```bash
//! cargo run --example unix_client --features "unix full-client"
//! ```

#[cfg(all(unix, feature = "unix"))]
use turbomcp::prelude::*;

#[cfg(all(unix, feature = "unix"))]
const SOCKET_PATH: &str = "/tmp/turbomcp-demo.sock";

#[derive(Clone)]
#[cfg(all(unix, feature = "unix"))]
struct UnixServer;

#[cfg(all(unix, feature = "unix"))]
#[turbomcp::server(name = "unix-demo", version = "1.0.0")]
impl UnixServer {
    #[tool("Echo a message")]
    async fn echo(&self, message: String) -> McpResult<String> {
        Ok(format!("Unix Echo: {}", message))
    }

    #[tool("Multiply two numbers")]
    async fn multiply(&self, a: f64, b: f64) -> McpResult<f64> {
        Ok(a * b)
    }

    #[resource("demo://status")]
    async fn status(&self, _uri: String, _ctx: &RequestContext) -> McpResult<String> {
        Ok(r#"{"transport":"unix","status":"ready"}"#.to_string())
    }
}

#[cfg(all(unix, feature = "unix"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("Unix server listening on {SOCKET_PATH}");

    UnixServer.run_unix(SOCKET_PATH).await?;

    Ok(())
}

#[cfg(not(all(unix, feature = "unix")))]
fn main() {
    eprintln!(
        "This example requires a Unix platform and the 'unix' feature. Run with: cargo run --example unix_server --features unix"
    );
}
