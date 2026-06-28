//! Schema normalization pre-pass.
//!
//! typify 0.6 panics on `2025-11-25/schema.json` with
//! `assertion failed: merged_schema.metadata.is_none()` (`convert.rs:1435`):
//! its `allOf`-merge path rejects metadata that survives the merge, and the MCP
//! task result types (`CancelTaskResult`, `GetTaskResult`,
//! `TaskStatusNotificationParams`) are `allOf[Base, Task]` where the `Task`
//! `$ref` target carries a `description`. See `.strategy/v4/AUDIT_FINDINGS.md`
//! F14.
//!
//! Fix: flatten every `allOf` intersection into a single inline object —
//! resolve each member (`$ref` → its definition, or inline subschema), drop
//! metadata at the merge site, and union `properties`/`required`/`type`/
//! `additionalProperties`. Semantically identical (an intersection of object
//! schemas is one object with all their fields), and typify never reaches the
//! offending merge path. Validated: 2025-11-25 → 25,473 lines, draft → 22,506,
//! both compile.

use serde_json::{Map, Value};

const METADATA_KEYS: [&str; 3] = ["description", "title", "default"];

/// Flatten every `allOf` in `schema`, in place. `$ref`s are resolved against a
/// snapshot of the schema's `$defs`/`definitions` taken before mutation.
pub fn flatten_all_of(schema: &mut Value) {
    let defs = schema
        .get("$defs")
        .or_else(|| schema.get("definitions"))
        .cloned()
        .unwrap_or(Value::Null);
    walk(schema, &defs);
}

fn walk(value: &mut Value, defs: &Value) {
    match value {
        Value::Object(map) => {
            if map.contains_key("allOf") {
                flatten_node(map, defs);
            }
            for child in map.values_mut() {
                walk(child, defs);
            }
        }
        Value::Array(items) => {
            for child in items {
                walk(child, defs);
            }
        }
        _ => {}
    }
}

/// Resolve a local `$ref` (e.g. `#/$defs/Task`) to a clone of its target.
fn resolve(defs: &Value, reference: &str) -> Option<Value> {
    let name = reference.rsplit('/').next()?;
    defs.get(name).cloned()
}

fn flatten_node(node: &mut Map<String, Value>, defs: &Value) {
    let members = match node.remove("allOf") {
        Some(Value::Array(members)) => members,
        // Not the shape we flatten — restore and leave it for typify.
        Some(other) => {
            node.insert("allOf".into(), other);
            return;
        }
        None => return,
    };

    let mut merged = Map::new();
    for member in members {
        let mut resolved = match member.get("$ref").and_then(Value::as_str) {
            Some(reference) => resolve(defs, reference).unwrap_or_else(|| member.clone()),
            None => member.clone(),
        };
        if let Value::Object(obj) = &mut resolved {
            // Defensive: a resolved target could itself be an intersection.
            if obj.contains_key("allOf") {
                flatten_node(obj, defs);
            }
            for key in METADATA_KEYS {
                obj.remove(key);
            }
            merge_object_into(&mut merged, obj);
        }
    }

    // Apply merged content without clobbering the node's own keys, so the
    // node's own `description`/`type` (the parent metadata) is preserved.
    for (key, value) in merged {
        node.entry(key).or_insert(value);
    }
}

fn merge_object_into(target: &mut Map<String, Value>, src: &Map<String, Value>) {
    if let Some(Value::Object(props)) = src.get("properties") {
        let entry = target
            .entry("properties")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(target_props) = entry {
            for (key, value) in props {
                target_props.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(Value::Array(required)) = src.get("required") {
        let entry = target
            .entry("required")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(target_required) = entry {
            for value in required {
                if !target_required.contains(value) {
                    target_required.push(value.clone());
                }
            }
        }
    }
    if let Some(ty) = src.get("type") {
        target.insert("type".into(), ty.clone());
    }
    if let Some(additional) = src.get("additionalProperties") {
        target
            .entry("additionalProperties")
            .or_insert_with(|| additional.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_allof_of_refs_and_drops_member_metadata() {
        let mut schema = json!({
            "$defs": {
                "Base": { "type": "object", "properties": { "a": { "type": "string" } } },
                "Task": {
                    "type": "object",
                    "description": "a task",
                    "properties": { "b": { "type": "number" } },
                    "required": ["b"]
                },
                "Result": {
                    "description": "the result",
                    "allOf": [ { "$ref": "#/$defs/Base" }, { "$ref": "#/$defs/Task" } ]
                }
            }
        });
        flatten_all_of(&mut schema);
        let result = &schema["$defs"]["Result"];
        assert!(result.get("allOf").is_none(), "allOf removed");
        // Parent metadata preserved.
        assert_eq!(result["description"], json!("the result"));
        // Member properties merged.
        assert!(result["properties"].get("a").is_some());
        assert!(result["properties"].get("b").is_some());
        assert_eq!(result["required"], json!(["b"]));
        assert_eq!(result["type"], json!("object"));
        // The standalone Task definition keeps its own description.
        assert_eq!(schema["$defs"]["Task"]["description"], json!("a task"));
    }
}
