//! Chrono-compatible serde support for [`jiff::SignedDuration`].
//!
//! # Why this module exists
//!
//! Before the jiff migration every duration in this workspace was a
//! `chrono::Duration` (now `chrono::TimeDelta`). `chrono` serializes a
//! duration as a two element array `[secs, nanos]`:
//!
//! | duration      | chrono output            |
//! |---------------|--------------------------|
//! | 24 hours      | `[86400,0]`              |
//! | 1.5 seconds   | `[1,500000000]`          |
//! | -1.5 seconds  | `[-2,500000000]`         |
//!
//! `chrono` keeps the fractional part unsigned (and in `0..1_000_000_000`)
//! and carries the sign in the seconds, i.e. it uses floor semantics:
//! `-1.5` seconds is `-2` seconds plus `500_000_000` nanoseconds.
//!
//! `jiff::SignedDuration` serializes as an ISO 8601 duration string
//! (`"PT24H"`), which is a different wire format. Durations that cross a
//! serde boundary therefore use the helpers in this module so the shape stays
//! exactly what `chrono` produced.
//!
//! # Usage
//!
//! ```rust
//! use jiff::SignedDuration;
//! use turbomcp_core::duration;
//!
//! let timeout = SignedDuration::from_hours(24);
//! assert_eq!(duration::to_parts(&timeout), (86_400, 0));
//! ```

use jiff::SignedDuration;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The number of nanoseconds in a second.
const NANOS_PER_SEC: i32 = 1_000_000_000;

/// Serialize a duration using chrono's `[secs, nanos]` shape.
///
/// Use with `#[serde(with = "turbomcp_core::duration")]`.
pub fn serialize<S>(duration: &SignedDuration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let (secs, nanos) = to_parts(duration);
    (secs, nanos).serialize(serializer)
}

/// Deserialize a duration from chrono's `[secs, nanos]` shape.
///
/// The nanosecond component has to be in `0..1_000_000_000`, exactly like
/// `chrono` required.
///
/// Use with `#[serde(with = "turbomcp_core::duration")]`.
pub fn deserialize<'de, D>(deserializer: D) -> Result<SignedDuration, D::Error>
where
    D: Deserializer<'de>,
{
    let (secs, nanos) = <(i64, i32) as Deserialize>::deserialize(deserializer)?;
    if !(0..NANOS_PER_SEC).contains(&nanos) {
        return Err(D::Error::custom(
            "duration out of bounds: nanoseconds must be in 0..1_000_000_000",
        ));
    }
    // `SignedDuration::new` applies the same floor semantics chrono used, so
    // `[-2, 500000000]` becomes -1.5 seconds.
    Ok(SignedDuration::new(secs, nanos))
}

/// Split a duration into chrono's `(secs, nanos)` parts.
///
/// `chrono` keeps the fractional part unsigned and carries the sign in the
/// seconds; `jiff` keeps the fractional part signed. This converts between
/// the two representations.
#[must_use]
pub fn to_parts(duration: &SignedDuration) -> (i64, i32) {
    let secs = duration.as_secs();
    let nanos = duration.subsec_nanos();
    if nanos < 0 {
        // Floor semantics: -1.5 seconds is -2 seconds plus 500_000_000
        // nanoseconds.
        (secs.saturating_sub(1), nanos + NANOS_PER_SEC)
    } else {
        (secs, nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::to_parts;
    use jiff::SignedDuration;

    /// Wrapper that uses the chrono compatible serde helpers.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Wrapper(#[serde(with = "super")] SignedDuration);

    #[test]
    fn parts_match_chrono() {
        assert_eq!(to_parts(&SignedDuration::from_hours(24)), (86_400, 0));
        assert_eq!(
            to_parts(&SignedDuration::from_millis(1500)),
            (1, 500_000_000)
        );
        assert_eq!(
            to_parts(&SignedDuration::from_millis(-1500)),
            (-2, 500_000_000)
        );
    }

    #[test]
    fn serializes_like_chrono() {
        let cases = [
            (SignedDuration::from_hours(24), "[86400,0]"),
            (SignedDuration::from_millis(1500), "[1,500000000]"),
            (SignedDuration::from_millis(-1500), "[-2,500000000]"),
        ];
        for (duration, expected) in cases {
            assert_eq!(serde_json::to_string(&Wrapper(duration)).unwrap(), expected);
        }
    }

    #[test]
    fn deserializes_like_chrono() {
        for (json, expected) in [
            ("[86400,0]", SignedDuration::from_hours(24)),
            ("[1,500000000]", SignedDuration::from_millis(1500)),
            ("[-2,500000000]", SignedDuration::from_millis(-1500)),
        ] {
            let parsed: Wrapper = serde_json::from_str(json).unwrap();
            assert_eq!(parsed.0, expected);
        }
    }

    #[test]
    fn rejects_out_of_range_nanoseconds() {
        assert!(serde_json::from_str::<Wrapper>("[0,-1]").is_err());
        assert!(serde_json::from_str::<Wrapper>("[0,1000000000]").is_err());
    }

    #[test]
    fn round_trips() {
        for duration in [
            SignedDuration::from_hours(24),
            SignedDuration::from_millis(1500),
            SignedDuration::from_millis(-1500),
            SignedDuration::ZERO,
            SignedDuration::from_nanos(-1),
        ] {
            let json = serde_json::to_string(&Wrapper(duration)).unwrap();
            let parsed: Wrapper = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.0, duration);
        }
    }
}
