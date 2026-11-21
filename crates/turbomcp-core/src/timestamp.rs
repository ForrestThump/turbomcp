//! Chrono-compatible RFC 3339 (de)serialization for [`jiff::Timestamp`].
//!
//! # Why this module exists
//!
//! Before the jiff migration every timestamp in this workspace was a
//! `chrono::DateTime<Utc>`. `chrono`'s serde implementation emits an RFC 3339
//! string in UTC with a *fixed width* fractional second component, chosen by
//! the `chrono::SecondsFormat::AutoSi` rule:
//!
//! | sub-second value | chrono output        |
//! |------------------|----------------------|
//! | `0`              | `2026-11-20T22:15:03Z`        |
//! | `100_000_000`    | `2026-11-20T22:15:03.100Z`    |
//! | `123_400_000`    | `2026-11-20T22:15:03.123400Z` |
//! | `123_456_789`    | `2026-11-20T22:15:03.123456789Z` |
//!
//! `jiff::Timestamp` emits the *shortest* fractional second component that
//! represents the value exactly, so `100_000_000` nanoseconds serialize as
//! `2026-11-20T22:15:03.1Z` and `123_400_000` nanoseconds serialize as
//! `2026-11-20T22:15:03.1234Z`.
//!
//! Both are valid RFC 3339, but they are different bytes on the wire. MCP
//! peers (and the golden JSON fixtures in this repository) were written
//! against the chrono representation, so every timestamp that crosses a serde
//! boundary uses the helpers in this module to keep the wire shape byte for
//! byte identical to the pre-migration output.
//!
//! # Leap seconds
//!
//! There is one case that cannot be preserved: a leap second
//! (`2016-12-31T23:59:60Z`). `jiff::Timestamp` cannot represent one, so the
//! adapter resolves it to the instant of the following second and renders
//! *that*:
//!
//! | input                     | chrono round trip             | adapter round trip            |
//! |---------------------------|-------------------------------|-------------------------------|
//! | `2016-12-31T23:59:60Z`    | `2016-12-31T23:59:60Z`        | `2017-01-01T00:00:00Z`        |
//! | `2016-12-31T23:59:60.5Z`  | `2016-12-31T23:59:60.500Z`    | `2017-01-01T00:00:00.500Z`    |
//!
//! The *instant* is the same in both cases, only the spelling of the leap
//! second is lost.
//!
//! # Usage
//!
//! ```rust
//! use turbomcp_core::timestamp;
//! use jiff::Timestamp;
//!
//! let ts = Timestamp::new(1_763_674_503, 100_000_000).unwrap();
//! assert_eq!(timestamp::format(&ts), "2025-11-20T21:35:03.100Z");
//! ```

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::{String, ToString};
use core::fmt;
use jiff::fmt::temporal::DateTimePrinter;
use jiff::tz::Offset;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserializer, Serializer};

/// RFC 3339 keeps at most nine fractional second digits.
const MAX_FRACTIONAL_DIGITS: usize = 9;

/// Printer for whole-second timestamps (`chrono`'s `SecondsFormat::Secs`).
const SECONDS: DateTimePrinter = DateTimePrinter::new().precision(Some(0));

/// Printer for millisecond timestamps (`chrono`'s `SecondsFormat::Millis`).
const MILLISECONDS: DateTimePrinter = DateTimePrinter::new().precision(Some(3));

/// Printer for microsecond timestamps (`chrono`'s `SecondsFormat::Micros`).
const MICROSECONDS: DateTimePrinter = DateTimePrinter::new().precision(Some(6));

/// Printer for nanosecond timestamps (`chrono`'s `SecondsFormat::Nanos`).
const NANOSECONDS: DateTimePrinter = DateTimePrinter::new().precision(Some(9));

/// Format a timestamp exactly like `chrono::DateTime<Utc>`'s serde output.
///
/// The result is an RFC 3339 string in UTC using the `Z` designator, with the
/// fractional second component rendered using chrono's `AutoSi` precision
/// rule (zero, three, six, or nine digits).
#[must_use]
pub fn format(timestamp: &Timestamp) -> String {
    let nanos = timestamp.subsec_nanosecond();
    let printer = precision_printer(nanos);

    let mut buffer = String::with_capacity(32);
    match printer.print_timestamp(timestamp, &mut buffer) {
        Ok(()) => buffer,
        // Writing into a `String` is infallible, so this branch is
        // unreachable. Fall back to jiff's own RFC 3339 rendering rather than
        // losing the timestamp entirely.
        Err(_) => alloc::format!("{timestamp}"),
    }
}

/// Format a timestamp exactly like `chrono::DateTime<Utc>`'s `to_rfc3339()`.
///
/// The result is an RFC 3339 string in UTC with a numeric `+00:00` offset
/// (chrono used `Z` for serde and `+00:00` for `to_rfc3339()`), using the same
/// `AutoSi` fractional second precision as [`format`].
#[must_use]
pub fn format_with_offset(timestamp: &Timestamp) -> String {
    let nanos = timestamp.subsec_nanosecond();
    let printer = precision_printer(nanos);

    let mut buffer = String::with_capacity(32);
    match printer.print_timestamp_with_offset(timestamp, Offset::UTC, &mut buffer) {
        Ok(()) => buffer,
        // Writing into a `String` is infallible, so this branch is
        // unreachable. Fall back to jiff's own RFC 3339 rendering rather than
        // losing the timestamp entirely.
        Err(_) => alloc::format!("{timestamp}"),
    }
}

/// Select the printer matching chrono's `SecondsFormat::AutoSi` rule.
fn precision_printer(nanos: i32) -> &'static DateTimePrinter {
    if nanos == 0 {
        &SECONDS
    } else if nanos % 1_000_000 == 0 {
        &MILLISECONDS
    } else if nanos % 1_000 == 0 {
        &MICROSECONDS
    } else {
        &NANOSECONDS
    }
}

/// Serialize a timestamp using the chrono-compatible representation.
///
/// Use with `#[serde(with = "turbomcp_core::timestamp")]`.
pub fn serialize<S>(timestamp: &Timestamp, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.collect_str(&format(timestamp))
}

/// Deserialize a timestamp from an RFC 3339 string.
///
/// Accepts everything `chrono::DateTime<Utc>` accepted: a `Z` designator, a
/// numeric UTC offset such as `+00:00` or `-05:00`, and an optional
/// fractional second component. Fractional seconds with more than nine
/// digits are truncated (chrono silently dropped the extra digits as well).
///
/// Use with `#[serde(with = "turbomcp_core::timestamp")]`.
pub fn deserialize<'de, D>(deserializer: D) -> Result<Timestamp, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(TimestampVisitor)
}

/// Serde helpers for `Option<Timestamp>`.
///
/// Use with `#[serde(with = "turbomcp_core::timestamp::option")]`.
pub mod option {
    use super::{deserialize as deserialize_timestamp, format};
    use core::fmt;
    use jiff::Timestamp;
    use serde::{Deserializer, Serializer};

    /// Serialize an optional timestamp using the chrono-compatible representation.
    pub fn serialize<S>(timestamp: &Option<Timestamp>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match timestamp {
            Some(timestamp) => serializer.collect_str(&format(timestamp)),
            None => serializer.serialize_none(),
        }
    }

    /// Deserialize an optional timestamp from an RFC 3339 string.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Timestamp>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OptionVisitor;

        impl<'de> serde::de::Visitor<'de> for OptionVisitor {
            type Value = Option<Timestamp>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an optional RFC 3339 timestamp string")
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(None)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(None)
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                deserialize_timestamp(deserializer).map(Some)
            }
        }

        deserializer.deserialize_option(OptionVisitor)
    }
}

/// Visitor accepting the string forms of an RFC 3339 timestamp.
struct TimestampVisitor;

impl<'de> serde::de::Visitor<'de> for TimestampVisitor {
    type Value = Timestamp;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an RFC 3339 timestamp string")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        parse(value).map_err(E::custom)
    }

    fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        match core::str::from_utf8(value) {
            Ok(value) => self.visit_str(value),
            Err(error) => Err(E::custom(error)),
        }
    }
}

/// Parse an RFC 3339 timestamp, accepting the same inputs as chrono.
fn parse(value: &str) -> Result<Timestamp, String> {
    if !has_chrono_rfc3339_shape(value) {
        return Err(format!("`{value}` is not an RFC 3339 timestamp"));
    }
    let (rewritten, leap_second) = rewrite_leap_second(value);
    let normalized = truncate_fractional_seconds(&rewritten);
    let timestamp = normalized
        .parse::<Timestamp>()
        .map_err(|error| error.to_string())?;
    if leap_second {
        // chrono resolved a leap second (`23:59:60`) to the instant of the
        // following second; jiff clamps it to `23:59:59`, so the second is
        // added back here.
        Ok(timestamp + SignedDuration::from_secs(1))
    } else {
        Ok(timestamp)
    }
}

/// Check that a value has the RFC 3339 shape `chrono` accepted.
///
/// `jiff` is more permissive than chrono: it also accepts an offset without
/// a colon (`+0000`), an offset with hours only (`+00`), an offset with
/// seconds (`+01:00:00`), a comma as the fractional second separator, the
/// basic format (`20261120T221503Z`), a time without seconds and trailing
/// annotations (`Z[UTC]`). None of those were accepted by chrono, so they
/// are rejected here to keep the set of accepted inputs identical.
fn has_chrono_rfc3339_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    // The shortest accepted value is `YYYY-MM-DDTHH:MM:SSZ`.
    if bytes.len() < 20 {
        return false;
    }
    // `YYYY-MM-DD`
    if !is_digits(&bytes[0..4])
        || bytes[4] != b'-'
        || !is_digits(&bytes[5..7])
        || bytes[7] != b'-'
        || !is_digits(&bytes[8..10])
    {
        return false;
    }
    // `T`, `t` or a space as the date/time separator.
    if !matches!(bytes[10], b'T' | b't' | b' ') {
        return false;
    }
    // `HH:MM:SS`
    if !is_digits(&bytes[11..13])
        || bytes[13] != b':'
        || !is_digits(&bytes[14..16])
        || bytes[16] != b':'
        || !is_digits(&bytes[17..19])
    {
        return false;
    }
    let mut index = 19;
    // An optional fractional second component, separated by a dot.
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let digits_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == digits_start {
            return false;
        }
    }
    // The offset: `Z`, `z` or a numeric `+HH:MM` / `-HH:MM`.
    match bytes.get(index) {
        Some(b'Z') | Some(b'z') => index + 1 == bytes.len(),
        Some(b'+') | Some(b'-') => {
            index + 6 == bytes.len()
                && is_digits(&bytes[index + 1..index + 3])
                && bytes[index + 3] == b':'
                && is_digits(&bytes[index + 4..index + 6])
        }
        _ => false,
    }
}

/// Check that a slice only contains ASCII digits.
fn is_digits(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit)
}

/// Rewrite a leap second (`23:59:60`) to `23:59:59`.
///
/// Returns the rewritten value and whether a leap second was present.
fn rewrite_leap_second(value: &str) -> (Cow<'_, str>, bool) {
    let Some(separator) = value.find(['T', 't', ' ']) else {
        return (Cow::Borrowed(value), false);
    };
    let time_start = separator + 1;
    let rest = &value[time_start..];
    let Some(hours) = rest.find(':') else {
        return (Cow::Borrowed(value), false);
    };
    let Some(minutes) = rest[hours + 1..].find(':') else {
        return (Cow::Borrowed(value), false);
    };
    // `HH:MM:SS` - the seconds start right after the second colon.
    let seconds_start = time_start + hours + 1 + minutes + 1;
    let seconds_end = seconds_start + 2;
    if value.get(seconds_start..seconds_end) != Some("60") {
        return (Cow::Borrowed(value), false);
    }

    let mut rewritten = String::with_capacity(value.len());
    rewritten.push_str(&value[..seconds_start]);
    rewritten.push_str("59");
    rewritten.push_str(&value[seconds_end..]);
    (Cow::Owned(rewritten), true)
}

/// Truncate a fractional second component to the nine digits RFC 3339 allows.
///
/// `chrono` accepted (and silently dropped) fractional seconds with more than
/// nine digits; `jiff` rejects them outright. Truncating keeps the set of
/// accepted inputs identical to the pre-migration behaviour.
fn truncate_fractional_seconds(value: &str) -> Cow<'_, str> {
    let Some(separator) = value.find(['T', 't', ' ']) else {
        return Cow::Borrowed(value);
    };
    let Some(dot) = value[separator..].find('.').map(|index| separator + index) else {
        return Cow::Borrowed(value);
    };

    let digits_start = dot + 1;
    let digits = value[digits_start..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if digits <= MAX_FRACTIONAL_DIGITS {
        return Cow::Borrowed(value);
    }

    // Keep the first nine digits and everything that follows the digit run.
    let digits_end = digits_start + digits;
    let keep_until = digits_start + MAX_FRACTIONAL_DIGITS;
    let mut truncated = String::with_capacity(value.len());
    truncated.push_str(&value[..keep_until]);
    truncated.push_str(&value[digits_end..]);
    Cow::Owned(truncated)
}

#[cfg(test)]
mod tests {
    use super::{format, parse, truncate_fractional_seconds};
    use jiff::Timestamp;

    #[test]
    fn whole_seconds_have_no_fractional_component() {
        let ts = Timestamp::new(1_763_674_503, 0).unwrap();
        assert_eq!(format(&ts), "2025-11-20T21:35:03Z");
    }

    #[test]
    fn fractional_component_uses_chrono_auto_si_precision() {
        let millis = Timestamp::new(1_763_674_503, 100_000_000).unwrap();
        assert_eq!(format(&millis), "2025-11-20T21:35:03.100Z");

        let micros = Timestamp::new(1_763_674_503, 123_400_000).unwrap();
        assert_eq!(format(&micros), "2025-11-20T21:35:03.123400Z");

        let nanos = Timestamp::new(1_763_674_503, 123_456_789).unwrap();
        assert_eq!(format(&nanos), "2025-11-20T21:35:03.123456789Z");
    }

    #[test]
    fn overlong_fractional_seconds_are_truncated() {
        assert_eq!(
            truncate_fractional_seconds("2025-11-20T21:35:03.1234567890Z"),
            "2025-11-20T21:35:03.123456789Z"
        );
        assert_eq!(
            truncate_fractional_seconds("2025-11-20T21:35:03.123Z"),
            "2025-11-20T21:35:03.123Z"
        );
        assert_eq!(
            truncate_fractional_seconds("2025-11-20T21:35:03Z"),
            "2025-11-20T21:35:03Z"
        );
    }

    #[test]
    fn overlong_fractional_seconds_are_parsed() {
        // chrono accepted (and dropped) the extra digits; jiff rejects them,
        // so the adapter truncates them first.
        let parsed = parse("2025-11-20T21:35:03.1234567890Z").expect("parses");
        assert_eq!(parsed, Timestamp::new(1_763_674_503, 123_456_789).unwrap());
    }

    #[test]
    fn leap_seconds_are_resolved_to_the_following_second() {
        // chrono parsed `23:59:60` as the instant of the next second, jiff
        // clamps it to `23:59:59`.
        let parsed = parse("2016-12-31T23:59:60Z").expect("leap second parses");
        assert_eq!(parsed, Timestamp::new(1_483_228_800, 0).unwrap());

        let parsed = parse("2016-12-31T23:59:60.5Z").expect("leap second parses");
        assert_eq!(parsed, Timestamp::new(1_483_228_800, 500_000_000).unwrap());

        let parsed = parse("2016-12-31T18:59:60-05:00").expect("leap second parses");
        assert_eq!(parsed, Timestamp::new(1_483_228_800, 0).unwrap());
    }

    #[test]
    fn inputs_chrono_accepted_are_accepted() {
        for value in [
            "2026-11-20T22:15:03Z",
            "2026-11-20T22:15:03z",
            "2026-11-20t22:15:03Z",
            "2026-11-20 22:15:03Z",
            "2026-11-20T22:15:03.5Z",
            "2026-11-20T22:15:03.123456789Z",
            "2026-11-20T22:15:03+00:00",
            "2026-11-20T22:15:03-05:00",
            "2026-11-20T22:15:03+05:30",
        ] {
            assert!(parse(value).is_ok(), "{value} should be accepted");
        }
    }

    #[test]
    fn inputs_chrono_rejected_are_rejected() {
        for value in [
            // An offset without a colon or with hours only.
            "2026-11-20T22:15:03+0000",
            "2026-11-20T22:15:03+00",
            // An offset with a seconds component.
            "2026-11-20T22:15:03+01:00:00",
            // A comma as the fractional second separator.
            "2026-11-20T22:15:03,5Z",
            // The basic format.
            "20261120T221503Z",
            // A trailing annotation.
            "2026-11-20T22:15:03Z[UTC]",
            // A time without seconds and a timestamp without an offset.
            "2026-11-20T22:15Z",
            "2026-11-20T22:15:03",
            // A negative year.
            "-000001-01-01T00:00:00Z",
        ] {
            assert!(parse(value).is_err(), "{value} should be rejected");
        }
    }

    #[test]
    fn leap_seconds_are_re_rendered_as_the_following_second() {
        // chrono kept the `:60` spelling when it rendered a leap second back
        // to the wire (`2016-12-31T23:59:60Z`). jiff cannot represent a leap
        // second, so the adapter resolves the instant and renders the
        // following second instead.
        let parsed = parse("2016-12-31T23:59:60Z").expect("leap second parses");
        assert_eq!(format(&parsed), "2017-01-01T00:00:00Z");

        let parsed = parse("2016-12-31T23:59:60.5Z").expect("leap second parses");
        assert_eq!(format(&parsed), "2017-01-01T00:00:00.500Z");
    }

    #[test]
    fn the_representable_range_is_accepted() {
        // `jiff::Timestamp::MAX` is `9999-12-30T22:00:00.999999999Z`; the
        // last two hours of that day are out of range and were accepted by
        // chrono.
        let max = "9999-12-30T22:00:00.999999999Z";
        assert_eq!(parse(max).expect("the maximum is accepted"), Timestamp::MAX);

        for value in [
            "9999-12-31T00:00:00Z",
            "9999-12-31T12:00:00Z",
            "9999-12-31T23:59:59.999999999Z",
        ] {
            assert!(parse(value).is_err(), "{value} is out of range");
        }
    }

    #[test]
    fn offsets_are_accepted() {
        let expected = Timestamp::new(1_763_674_503, 0).unwrap();
        assert_eq!(parse("2025-11-20T21:35:03Z").unwrap(), expected);
        assert_eq!(parse("2025-11-20T21:35:03+00:00").unwrap(), expected);
        assert_eq!(parse("2025-11-20T16:35:03-05:00").unwrap(), expected);
        assert_eq!(parse("2025-11-21T03:05:03+05:30").unwrap(), expected);
    }
}
