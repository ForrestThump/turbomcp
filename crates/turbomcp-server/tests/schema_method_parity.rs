use std::collections::BTreeSet;

use serde_json::Value;
use turbomcp_protocol::versioning::adapter::adapter_for_version;
use turbomcp_types::ProtocolVersion;

fn collect_rpc_methods(schema: &Value) -> BTreeSet<String> {
    let mut methods = BTreeSet::new();
    collect_rpc_methods_inner(schema, &mut methods);
    methods
}

fn collect_rpc_methods_inner(value: &Value, methods: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(method) = map.get("const").and_then(Value::as_str)
                && (method == "initialize"
                    || method == "ping"
                    || method.starts_with("tools/")
                    || method.starts_with("resources/")
                    || method.starts_with("prompts/")
                    || method.starts_with("completion/")
                    || method.starts_with("logging/")
                    || method.starts_with("notifications/")
                    || method.starts_with("roots/")
                    || method.starts_with("sampling/")
                    || method.starts_with("elicitation/")
                    || method.starts_with("tasks/"))
            {
                methods.insert(method.to_string());
            }

            for child in map.values() {
                collect_rpc_methods_inner(child, methods);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_rpc_methods_inner(child, methods);
            }
        }
        _ => {}
    }
}

fn adapter_methods(version: ProtocolVersion) -> BTreeSet<String> {
    adapter_for_version(&version)
        .supported_methods()
        .iter()
        .map(|method| (*method).to_string())
        .collect()
}

#[test]
fn version_adapter_methods_match_embedded_stable_schemas() {
    let cases = [
        (
            ProtocolVersion::V2025_06_18,
            include_str!("../src/schemas/mcp_2025-06-18.json"),
        ),
        (
            ProtocolVersion::V2025_11_25,
            include_str!("../src/schemas/mcp_2025-11-25.json"),
        ),
    ];

    for (version, schema) in cases {
        let schema: Value = serde_json::from_str(schema).expect("valid embedded schema");
        assert_eq!(
            adapter_methods(version.clone()),
            collect_rpc_methods(&schema),
            "version adapter method set must match embedded schema for {version}"
        );
    }
}
