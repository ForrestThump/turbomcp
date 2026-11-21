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
use alloc::string::String;
use core::fmt;
use jiff::Timestamp;
use jiff::fmt::temporal::DateTimePrinter;
use jiff::tz::Offset;
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
fn parse(value: &str) -> Result<Timestamp, jiff::Error> {
    let normalized = truncate_fractional_seconds(value);
    normalized.parse::<Timestamp>()
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
    fn offsets_are_accepted() {
        let expected = Timestamp::new(1_763_674_503, 0).unwrap();
        assert_eq!(parse("2025-11-20T21:35:03Z").unwrap(), expected);
        assert_eq!(parse("2025-11-20T21:35:03+00:00").unwrap(), expected);
        assert_eq!(parse("2025-11-20T16:35:03-05:00").unwrap(), expected);
        assert_eq!(parse("2025-11-21T03:05:03+05:30").unwrap(), expected);
    }
}
