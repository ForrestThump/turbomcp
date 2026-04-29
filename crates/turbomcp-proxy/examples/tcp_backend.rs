//! Example: TCP Backend Proxy
//!
//! Demonstrates connecting to an MCP server via TCP and printing its capabilities.
//!
//! Usage:
//!   1. Start an MCP server on TCP port 8765
//!   2. Run: cargo run --example tcp_backend
//!   3. Inspect the backend capabilities printed by this example
//!
//! Example MCP server startup:
//!   ```bash
//!   cargo run -p turbomcp --example tcp_server --features tcp
//!   ```

use turbomcp_proxy::proxy::{BackendConfig, BackendConnector, BackendTransport};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing for logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("🚀 TCP Backend Proxy Example");
    println!("============================\n");

    // Configure TCP backend connection
    let backend_config = BackendConfig {
        transport: BackendTransport::Tcp {
            host: "127.0.0.1".to_string(),
            port: 8765,
        },
        client_name: "tcp-proxy-example".to_string(),
        client_version: "1.0.0".to_string(),
    };

    println!("📡 Connecting to TCP server at 127.0.0.1:8765...");

    // Create backend connector (establishes connection and initializes)
    let backend = BackendConnector::new(backend_config).await?;
    println!("✅ Connected to backend successfully");

    // Introspect server capabilities
    println!("\n🔍 Introspecting server capabilities...");
    let spec = backend.introspect().await?;

    println!("✅ Introspection complete");
    println!("   Server: {}", spec.server_info.name);
    println!("   Version: {}", spec.server_info.version);
    println!("   Tools: {}", spec.tools.len());
    println!("   Resources: {}", spec.resources.len());
    println!("   Prompts: {}", spec.prompts.len());

    // List available tools
    if !spec.tools.is_empty() {
        println!("\n📋 Available Tools:");
        for tool in &spec.tools {
            println!("   - {}", tool.name);
            if let Some(desc) = &tool.description {
                println!("     {}", desc);
            }
        }
    }

    // List available resources
    if !spec.resources.is_empty() {
        println!("\n📂 Available Resources:");
        for resource in &spec.resources {
            println!("   - {}", resource.uri);
            if let Some(desc) = &resource.description {
                println!("     {}", desc);
            }
        }
    }

    println!("\n✨ TCP backend proxy is ready!");
    println!("In a production scenario, you would now:");
    println!("  1. Wrap this backend in a ProxyService");
    println!("  2. Expose it over HTTP with Axum");
    println!(
        "  3. Run: turbomcp-proxy serve --backend tcp --tcp 127.0.0.1:8765 --frontend http --bind 127.0.0.1:3001"
    );

    Ok(())
}
