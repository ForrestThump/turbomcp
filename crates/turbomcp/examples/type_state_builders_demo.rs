//! # Type-State Capability Builders Demo
//!
//! This example demonstrates TurboMCP's const-generic type-state builders
//! that provide compile-time validation of capability configurations with
//! zero-cost abstractions and advanced safety features.

use turbomcp_protocol::capabilities::builders::{
    ClientCapabilitiesBuilder, ServerCapabilitiesBuilder,
};

fn main() {
    println!("🚀 TurboMCP Type-State Capability Builders Demo");
    println!("===============================================\n");

    // Example 1: Server capabilities with compile-time validation
    println!("1. Server Capabilities with Type-State Validation");
    println!("   -----------------------------------------------");

    let server_caps = ServerCapabilitiesBuilder::new()
        .enable_experimental() // Enables experimental capability state
        .enable_tools() // Enables tools capability state
        .enable_prompts() // Enables prompts capability state
        .enable_resources() // Enables resources capability state
        // These methods are only available because we enabled the parent capabilities!
        .enable_tool_list_changed() // ✅ Only available when tools enabled
        .enable_prompts_list_changed() // ✅ Only available when prompts enabled
        .enable_resources_list_changed() // ✅ Only available when resources enabled
        .enable_resources_subscribe() // ✅ Only available when resources enabled
        // TurboMCP exclusive features!
        .with_simd_optimization("avx2") // 🚀 TurboMCP exclusive
        .with_enterprise_security(true) // 🚀 TurboMCP exclusive
        .build();

    println!("   ✅ Server capabilities configured with compile-time validation");
    println!("   📊 Tools enabled: {}", server_caps.tools.is_some());
    println!("   📝 Prompts enabled: {}", server_caps.prompts.is_some());
    println!(
        "   📚 Resources enabled: {}",
        server_caps.resources.is_some()
    );
    println!(
        "   🧪 Experimental features: {}",
        server_caps.experimental.as_ref().map_or(0, |e| e.len())
    );

    // Example 2: Client capabilities with opt-out model
    println!("\n2. Opt-Out Capability Model (Forward Compatible!)");
    println!("   -----------------------------------------------");

    // By default, ALL capabilities are enabled!
    let client_caps = ClientCapabilitiesBuilder::new()
        .enable_roots_list_changed() // Configure sub-capabilities
        .build();

    println!("   ✅ All capabilities enabled by default (opt-out model)");
    println!("   🗂️  Roots enabled: {}", client_caps.roots.is_some());
    println!("   🎯 Sampling enabled: {}", client_caps.sampling.is_some());
    println!(
        "   🤝 Elicitation enabled: {}",
        client_caps.elicitation.is_some()
    );
    println!(
        "   🧪 Experimental enabled: {}",
        client_caps.experimental.is_some()
    );

    // Example 2b: Selective disable (opt-out pattern)
    println!("\n2b. Selectively Disable Capabilities");
    println!("    ----------------------------------");

    let restricted_client = ClientCapabilitiesBuilder::new()
        .without_elicitation() // Disable user prompts
        .without_experimental() // Disable experimental features
        .build();

    println!("   ✅ Disabled elicitation and experimental");
    println!(
        "   🗂️  Roots enabled: {}",
        restricted_client.roots.is_some()
    );
    println!(
        "   🎯 Sampling enabled: {}",
        restricted_client.sampling.is_some()
    );
    println!(
        "   🤝 Elicitation disabled: {}",
        restricted_client.elicitation.is_none()
    );
    println!(
        "   🧪 Experimental disabled: {}",
        restricted_client.experimental.is_none()
    );

    // Example 3: Building servers with explicit capability selection
    println!("\n3. Building Servers with Explicit Capabilities");
    println!("   -------------------------------------------");

    // Full-featured server - explicitly enable everything you need
    let full_server = ServerCapabilitiesBuilder::new()
        .enable_experimental()
        .enable_logging()
        .enable_completions()
        .enable_prompts()
        .enable_resources()
        .enable_tools()
        .enable_tool_list_changed()
        .enable_prompts_list_changed()
        .enable_resources_list_changed()
        .enable_resources_subscribe()
        .build();
    println!(
        "   🚀 Full-featured server: {} capabilities enabled",
        count_server_capabilities(&full_server)
    );

    // Minimal server - just enable what you need
    let minimal_server = ServerCapabilitiesBuilder::new().enable_tools().build();
    println!(
        "   ⚡ Minimal server: {} capabilities enabled",
        count_server_capabilities(&minimal_server)
    );

    // Example 4: Opt-in pattern with minimal()
    println!("\n4. Opt-In Pattern (For Restrictive Clients)");
    println!("   -----------------------------------------");

    let minimal_client = ClientCapabilitiesBuilder::minimal()
        .enable_sampling() // Only enable what we need
        .enable_roots()
        .build();

    println!("   ✅ Minimal client starts with nothing enabled");
    println!("   🗂️  Roots enabled: {}", minimal_client.roots.is_some());
    println!(
        "   🎯 Sampling enabled: {}",
        minimal_client.sampling.is_some()
    );
    println!(
        "   🤝 Elicitation disabled: {}",
        minimal_client.elicitation.is_none()
    );
    println!(
        "   🧪 Experimental disabled: {}",
        minimal_client.experimental.is_none()
    );

    println!("\n5. TurboMCP Exclusive Features");
    println!("   ----------------------------");

    // Show TurboMCP-specific experimental features
    if let Some(ref experimental) = server_caps.experimental {
        println!("   🚀 TurboMCP Server Extensions:");
        for (key, value) in experimental {
            if key.starts_with("turbomcp_") {
                println!("      - {}: {}", key.replace("turbomcp_", ""), value);
            }
        }
    }

    if let Some(ref experimental) = client_caps.experimental {
        let turbomcp_extensions: Vec<_> = experimental
            .iter()
            .filter(|(key, _)| key.starts_with("turbomcp_"))
            .collect();

        if !turbomcp_extensions.is_empty() {
            println!("   🚀 TurboMCP Client Extensions:");
            for (key, value) in turbomcp_extensions {
                println!("      - {}: {}", key.replace("turbomcp_", ""), value);
            }
        }
    }

    println!("\n🎉 Demo Complete! TurboMCP capability builders provide:");
    println!("   ✅ Opt-out model (forward compatible!)");
    println!("   ✅ Compile-time capability validation");
    println!("   ✅ Advanced MCP capability support");
    println!("   ✅ Opt-in pattern via minimal()");
    println!("   ✅ Zero-cost abstractions");
    println!("\n🏆 TurboMCP: Future-proof capability negotiation!");
}

/// Count enabled server capabilities
fn count_server_capabilities(caps: &turbomcp_protocol::types::ServerCapabilities) -> usize {
    let mut count = 0;
    if caps.experimental.is_some() {
        count += 1;
    }
    if caps.logging.is_some() {
        count += 1;
    }
    if caps.completions.is_some() {
        count += 1;
    }
    if caps.prompts.is_some() {
        count += 1;
    }
    if caps.resources.is_some() {
        count += 1;
    }
    if caps.tools.is_some() {
        count += 1;
    }
    count
}

/// Count enabled client capabilities
#[allow(dead_code)]
fn count_client_capabilities(caps: &turbomcp_protocol::types::ClientCapabilities) -> usize {
    let mut count = 0;
    if caps.experimental.is_some() {
        count += 1;
    }
    if caps.roots.is_some() {
        count += 1;
    }
    if caps.sampling.is_some() {
        count += 1;
    }
    if caps.elicitation.is_some() {
        count += 1;
    }
    count
}
