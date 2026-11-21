//! Wire-compatibility tests for the chrono -> jiff migration.
//!
//! The MCP protocol types that carry a timestamp are serialized into
//! JSON-RPC payloads, so their byte layout must not change when the
//! implementation swaps `chrono::DateTime<Utc>` for `jiff::Timestamp`.
//!
//! The expected strings in this file were produced by `chrono 0.4.44`'s
//! `serde` implementation (`DateTime<Utc>` -> RFC 3339 in UTC, `Z`
//! designator, fractional second component sized by chrono's
//! `SecondsFormat::AutoSi` rule).

use jiff::Timestamp;
use serde_json::json;
use turbomcp_protocol::session::{SessionEvent, SessionEventType};
use turbomcp_protocol::types::Timestamp as ProtocolTimestamp;
use turbomcp_protocol::{ClientSession, ConnectionMetrics, RequestInfo};

/// 2026-11-20T22:15:03Z
const BASE_SECOND: i64 = 1_795_212_903;

/// Build a timestamp with the given nanosecond component.
fn timestamp(nanos: i32) -> Timestamp {
    Timestamp::new(BASE_SECOND, nanos).expect("valid timestamp")
}

#[test]
fn protocol_timestamp_matches_chrono_json_for_whole_seconds() {
    let value = ProtocolTimestamp(timestamp(0));
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        "\"2026-11-20T22:15:03Z\""
    );
}

#[test]
fn protocol_timestamp_matches_chrono_json_for_milliseconds() {
    let value = ProtocolTimestamp(timestamp(100_000_000));
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        "\"2026-11-20T22:15:03.100Z\""
    );
}

#[test]
fn protocol_timestamp_matches_chrono_json_for_microseconds() {
    let value = ProtocolTimestamp(timestamp(123_400_000));
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        "\"2026-11-20T22:15:03.123400Z\""
    );
}

#[test]
fn protocol_timestamp_matches_chrono_json_for_nanoseconds() {
    let value = ProtocolTimestamp(timestamp(123_456_789));
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        "\"2026-11-20T22:15:03.123456789Z\""
    );
}

#[test]
fn request_info_timestamp_matches_chrono_json() {
    let mut info = RequestInfo::new("client-1".into(), "tools/list".into(), json!({}));
    info.timestamp = timestamp(123_400_000);
    let value = serde_json::to_value(&info).unwrap();
    assert_eq!(value["timestamp"], json!("2026-11-20T22:15:03.123400Z"));
}

#[test]
fn session_event_timestamp_matches_chrono_json() {
    let event = SessionEvent {
        timestamp: timestamp(100_000_000),
        client_id: "client-1".into(),
        event_type: SessionEventType::Created,
        metadata: Default::default(),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["timestamp"], json!("2026-11-20T22:15:03.100Z"));
}

#[test]
fn client_session_timestamps_match_chrono_json() {
    let mut session = ClientSession::new("client-1".into(), "websocket".into());
    session.connected_at = timestamp(0);
    session.last_activity = timestamp(123_456_789);

    let value = serde_json::to_value(&session).unwrap();
    assert_eq!(value["connected_at"], json!("2026-11-20T22:15:03Z"));
    assert_eq!(
        value["last_activity"],
        json!("2026-11-20T22:15:03.123456789Z")
    );
}

#[test]
fn optional_connection_metric_timestamp_matches_chrono_json() {
    let metrics = ConnectionMetrics {
        rtt_ms: Some(1.0),
        packet_loss: Some(0.0),
        uptime_seconds: Some(2),
        bytes_sent: Some(3),
        bytes_received: Some(4),
        last_success: Some(timestamp(123_400_000)),
    };
    let value = serde_json::to_value(&metrics).unwrap();
    assert_eq!(value["last_success"], json!("2026-11-20T22:15:03.123400Z"));

    let metrics = ConnectionMetrics {
        last_success: None,
        ..metrics
    };
    let value = serde_json::to_value(&metrics).unwrap();
    assert!(value["last_success"].is_null());
}

#[test]
fn protocol_timestamp_parses_zulu_designator() {
    let parsed: ProtocolTimestamp =
        serde_json::from_str("\"2026-11-20T22:15:03.123456Z\"").unwrap();
    assert_eq!(parsed.0, timestamp(123_456_000));
}

#[test]
fn protocol_timestamp_parses_explicit_utc_offset() {
    let parsed: ProtocolTimestamp =
        serde_json::from_str("\"2026-11-20T22:15:03.123456+00:00\"").unwrap();
    assert_eq!(parsed.0, timestamp(123_456_000));
}

#[test]
fn protocol_timestamp_parses_non_utc_offset() {
    let parsed: ProtocolTimestamp =
        serde_json::from_str("\"2026-11-20T17:15:03.123456-05:00\"").unwrap();
    assert_eq!(parsed.0, timestamp(123_456_000));

    let parsed: ProtocolTimestamp =
        serde_json::from_str("\"2026-11-21T03:45:03.123456+05:30\"").unwrap();
    assert_eq!(parsed.0, timestamp(123_456_000));
}

#[test]
fn protocol_timestamp_rejects_a_missing_offset() {
    assert!(serde_json::from_str::<ProtocolTimestamp>("\"2026-11-20T22:15:03\"").is_err());
}

#[test]
fn protocol_timestamp_display_uses_the_chrono_rfc3339_layout() {
    let value = ProtocolTimestamp(timestamp(123_400_000));
    assert_eq!(value.to_string(), "2026-11-20T22:15:03.123400Z");
}

#[test]
fn protocol_timestamp_round_trips_through_json() {
    for nanos in [0, 100_000_000, 123_400_000, 123_456_789] {
        let value = ProtocolTimestamp(timestamp(nanos));
        let json = serde_json::to_string(&value).unwrap();
        let parsed: ProtocolTimestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, value);
    }
}

#[test]
fn elapsed_is_a_signed_duration() {
    let value = ProtocolTimestamp(timestamp(0));
    let elapsed = value.elapsed();
    // The fixture is in the past, so the elapsed time is positive.
    assert!(elapsed.as_secs() > 0);
}

#[test]
fn session_duration_and_idle_use_signed_durations() {
    let mut session = ClientSession::new("client-1".into(), "websocket".into());
    let now = Timestamp::now();
    session.connected_at = now - jiff::SignedDuration::from_secs(120);
    session.last_activity = now - jiff::SignedDuration::from_secs(60);

    assert_eq!(
        session.session_duration(),
        jiff::SignedDuration::from_secs(60)
    );
    assert!(!session.is_idle(jiff::SignedDuration::from_secs(120)));
    assert!(session.is_idle(jiff::SignedDuration::from_secs(30)));
}

#[test]
fn integer_second_semantics_match_chrono_timestamp() {
    use std::time::{SystemTime, UNIX_EPOCH};

    // `chrono::Utc::now().timestamp()` and `jiff::Timestamp::now().as_second()`
    // both report the number of whole seconds since the Unix epoch.
    let now = Timestamp::now();
    let system = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_secs() as i64;
    assert!((now.as_second() - system).abs() <= 1);

    // A positive timestamp truncates its sub-second component towards zero,
    // exactly like chrono.
    assert_eq!(
        jiff::Timestamp::new(1_795_212_903, 500_000_000)
            .unwrap()
            .as_second(),
        1_795_212_903
    );

    // Before the Unix epoch chrono floored while jiff truncates towards
    // zero. Nothing in this workspace timestamps the past, but the
    // difference is pinned here so it cannot surprise a future caller.
    assert_eq!(
        jiff::Timestamp::new(-1, 500_000_000).unwrap().as_second(),
        0
    );
}

#[test]
fn civil_date_validation_keeps_chrono_leniency() {
    use turbomcp_protocol::validation::ProtocolValidator;

    // Strict ISO 8601 shape.
    assert!(ProtocolValidator::validate_string_format("2025-10-07", "date").is_ok());
    assert!(ProtocolValidator::validate_string_format("2025-01-01", "date").is_ok());

    // Range checks (month/day/leap year).
    assert!(ProtocolValidator::validate_string_format("2025-13-01", "date").is_err());
    assert!(ProtocolValidator::validate_string_format("2025-02-30", "date").is_err());
    assert!(ProtocolValidator::validate_string_format("10/07/2025", "date").is_err());
    assert!(ProtocolValidator::validate_string_format("not-a-date", "date").is_err());

    // `chrono`'s `%Y-%m-%d` accepted a one digit month/day, and so does the
    // migration.
    assert!(ProtocolValidator::validate_string_format("2025-1-7", "date").is_ok());

    // Leap day is only valid in a leap year.
    assert!(ProtocolValidator::validate_string_format("2024-02-29", "date").is_ok());
    assert!(ProtocolValidator::validate_string_format("2025-02-29", "date").is_err());
}

#[test]
fn month_end_and_leap_day_arithmetic_follow_the_calendar() {
    use jiff::Span;
    use jiff::civil::Date;

    // January 31 + 1 month lands on the last day of February.
    let january_31 = Date::new(2026, 1, 31).unwrap();
    let february = january_31.checked_add(Span::new().months(1)).unwrap();
    assert_eq!(february, Date::new(2026, 2, 28).unwrap());

    // February 29 (leap year) + 1 year lands on February 28.
    let leap_day = Date::new(2024, 2, 29).unwrap();
    let next_year = leap_day.checked_add(Span::new().years(1)).unwrap();
    assert_eq!(next_year, Date::new(2025, 2, 28).unwrap());

    // February 29 + 4 years is still a leap day.
    let leap_day = Date::new(2024, 2, 29).unwrap();
    let four_years = leap_day.checked_add(Span::new().years(4)).unwrap();
    assert_eq!(four_years, Date::new(2028, 2, 29).unwrap());
}

#[test]
fn daylight_saving_transitions_do_not_disturb_utc_timestamps() {
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    let chicago = TimeZone::get("America/Chicago").expect("tzdb is available");

    // 2026-03-08 02:30 America/Chicago does not exist (spring forward).
    // jiff resolves it to the first valid instant after the gap.
    let gap = date(2026, 3, 8).at(2, 30, 0, 0);
    let resolved = chicago.to_zoned(gap).expect("gap is resolved");
    assert_eq!(resolved.datetime().hour(), 3);
    assert_eq!(resolved.offset().seconds(), -5 * 3600);
    assert_eq!(
        resolved.timestamp(),
        "2026-03-08T08:30:00Z".parse::<Timestamp>().unwrap()
    );

    // 2026-11-01 01:30 America/Chicago is ambiguous (fall back). jiff picks
    // the earlier (daylight saving) occurrence by default and the later
    // (standard time) occurrence is exactly one hour further along.
    let ambiguous = date(2026, 11, 1).at(1, 30, 0, 0);
    let ambiguous = chicago.to_ambiguous_zoned(ambiguous);
    assert!(ambiguous.is_ambiguous());

    let earlier = ambiguous.clone().earlier().expect("ambiguity resolved");
    let later = ambiguous.later().expect("ambiguity resolved");
    assert_eq!(earlier.offset().seconds(), -5 * 3600);
    assert_eq!(later.offset().seconds(), -6 * 3600);
    assert_eq!(
        later.timestamp().as_second() - earlier.timestamp().as_second(),
        3600
    );

    // The default (compatible) resolution is the earlier occurrence.
    let default = chicago.to_zoned(date(2026, 11, 1).at(1, 30, 0, 0)).unwrap();
    assert_eq!(default.timestamp(), earlier.timestamp());
}
