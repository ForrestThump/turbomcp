//! JSON schema helpers for the chrono compatible representations.
//!
//! The durations of this workspace (for example
//! `turbomcp_protocol::session::SessionConfig::session_timeout`) are
//! serialized as the two element array `[secs, nanos]` that chrono used,
//! while `jiff::SignedDuration` itself serializes as an ISO 8601 duration
//! string (`"PT24H"`). A derived JSON schema therefore has to be pointed at
//! the helpers in this module so that the schema matches the serialized
//! value.

use schemars::{Schema, SchemaGenerator};

/// The JSON schema of a duration in chrono's `[secs, nanos]` shape.
///
/// The schema is the one `schemars` generates for `chrono::TimeDelta`.
///
/// # Example
///
/// ```rust,ignore
/// #[derive(schemars::JsonSchema)]
/// struct MyConfig {
///     #[schemars(schema_with = "turbomcp::schema::duration")]
///     session_timeout: jiff::SignedDuration,
/// }
/// ```
pub fn duration(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "array",
        "prefixItems": [
            {
                "type": "integer",
                "format": "int64"
            },
            {
                "type": "integer",
                "minimum": 0,
                "exclusiveMaximum": 1_000_000_000
            }
        ],
        "minItems": 2,
        "maxItems": 2
    })
}

#[cfg(test)]
mod tests {
    /// A configuration with a duration, described with the chrono compatible
    /// schema.
    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct Config {
        #[serde(with = "turbomcp_core::duration")]
        #[schemars(schema_with = "crate::schema::duration")]
        session_timeout: jiff::SignedDuration,
    }

    #[test]
    fn duration_schema_is_the_chrono_array() {
        let schema = schemars::schema_for!(Config);
        let value = serde_json::to_value(&schema).expect("schema serializes");
        let duration = &value["properties"]["session_timeout"];

        assert_eq!(duration["type"], serde_json::json!("array"));
        assert_eq!(
            duration["prefixItems"][0]["type"],
            serde_json::json!("integer")
        );
        assert_eq!(
            duration["prefixItems"][0]["format"],
            serde_json::json!("int64")
        );
        assert_eq!(
            duration["prefixItems"][1]["type"],
            serde_json::json!("integer")
        );
        assert_eq!(duration["prefixItems"][1]["minimum"], serde_json::json!(0));
        assert_eq!(
            duration["prefixItems"][1]["exclusiveMaximum"],
            serde_json::json!(1_000_000_000)
        );
        assert_eq!(duration["minItems"], serde_json::json!(2));
        assert_eq!(duration["maxItems"], serde_json::json!(2));

        // The schema describes the shape the value is serialized in.
        let config = Config {
            session_timeout: jiff::SignedDuration::from_hours(24),
        };
        let value = serde_json::to_value(&config).expect("config serializes");
        assert_eq!(value["session_timeout"], serde_json::json!([86_400, 0]));
    }
}
