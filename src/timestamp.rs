//! [`Timestamp`]: the second an envelope or a stream event was written, and
//! its RFC 3339 UTC spelling on the wire.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use serde::{Serialize, Serializer};

/// A second between 1970-01-01T00:00:00Z and 9999-12-31T23:59:59Z, the range
/// a four-digit-year RFC 3339 UTC timestamp can name.
///
/// Displays and serializes as RFC 3339 UTC (`2026-06-10T14:42:17Z`), so an
/// agent reads the current date directly instead of epoch seconds. Parses
/// back from exactly that shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp(u64);

impl Timestamp {
    /// 1970-01-01T00:00:00Z.
    pub const UNIX_EPOCH: Self = Self(0);
    /// 9999-12-31T23:59:59Z, the last second a four-digit year can name.
    pub const MAX: Self = Self(253_402_300_799);

    /// The timestamp `secs` seconds after the Unix epoch, or `None` past
    /// [`Timestamp::MAX`].
    #[must_use]
    pub const fn from_epoch_secs(secs: u64) -> Option<Self> {
        if secs <= Self::MAX.0 {
            Some(Self(secs))
        } else {
            None
        }
    }

    /// Seconds since the Unix epoch.
    #[must_use]
    pub const fn epoch_secs(self) -> u64 {
        self.0
    }
}

/// RFC 3339 UTC, e.g. `2026-06-10T14:42:17Z`.
///
/// Hand-rolled (Howard Hinnant's civil-from-days algorithm) so the dependency
/// set stays `serde`/`serde_json`.
impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let epoch = self.0;
        let secs_of_day = epoch % 86_400;
        let (hh, mm, ss) = (
            secs_of_day / 3_600,
            secs_of_day % 3_600 / 60,
            secs_of_day % 60,
        );

        // epoch is unsigned, so plain division suffices: no pre-1970 date can
        // reach here.
        let z = epoch / 86_400 + 719_468;
        let era = z / 146_097;
        let doe = z % 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + u64::from(month <= 2);

        write!(f, "{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
    }
}

/// A string that is not an RFC 3339 UTC timestamp in the one shape
/// [`Timestamp`] displays as (`YYYY-MM-DDTHH:MM:SSZ`, from 1970 on).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseTimestampError {
    input: String,
}

impl fmt::Display for ParseTimestampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "not an RFC 3339 UTC timestamp (YYYY-MM-DDTHH:MM:SSZ): {:?}",
            self.input
        )
    }
}

impl Error for ParseTimestampError {}

/// Strict: exactly `YYYY-MM-DDTHH:MM:SSZ`, ASCII digits only, a real
/// calendar date from 1970 on. No offsets, fractions or leap seconds.
impl FromStr for Timestamp {
    type Err = ParseTimestampError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse(s.as_bytes()).ok_or_else(|| ParseTimestampError {
            input: s.to_string(),
        })
    }
}

/// The epoch second `bytes` names, or `None` if it is not the exact shape.
fn parse(bytes: &[u8]) -> Option<Timestamp> {
    const SHAPE: &[u8; 20] = b"dddd-dd-ddTdd:dd:ddZ";
    if bytes.len() != SHAPE.len() {
        return None;
    }
    let shaped = bytes.iter().zip(SHAPE).all(|(&byte, &expected)| {
        if expected == b'd' {
            byte.is_ascii_digit()
        } else {
            byte == expected
        }
    });
    if !shaped {
        return None;
    }
    let number = |start: usize, end: usize| {
        bytes[start..end]
            .iter()
            .fold(0, |total, digit| total * 10 + u64::from(digit - b'0'))
    };
    let (year, month, day) = (number(0, 4), number(5, 7), number(8, 10));
    let (hh, mm, ss) = (number(11, 13), number(14, 16), number(17, 19));
    if year < 1970 || !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day)
    {
        return None;
    }
    if hh > 23 || mm > 59 || ss > 59 {
        return None;
    }

    // Hinnant's days_from_civil, the inverse of the Display impl above.
    let y = year - u64::from(month <= 2);
    let era = y / 400;
    let yoe = y % 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    // A four-digit year never passes MAX, so this always holds a value.
    Timestamp::from_epoch_secs(days * 86_400 + hh * 3_600 + mm * 60 + ss)
}

/// The number of days in `month` (1-12) of `year`.
fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(feature = "deserialize")]
impl<'de> serde::Deserialize<'de> for Timestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> Timestamp {
        Timestamp::from_epoch_secs(secs).expect("in range")
    }

    #[test]
    fn range_is_the_four_digit_years() {
        assert_eq!(Timestamp::UNIX_EPOCH.to_string(), "1970-01-01T00:00:00Z");
        assert_eq!(Timestamp::MAX.to_string(), "9999-12-31T23:59:59Z");
        assert_eq!(
            Timestamp::from_epoch_secs(Timestamp::MAX.epoch_secs()),
            Some(Timestamp::MAX)
        );
        assert_eq!(
            Timestamp::from_epoch_secs(Timestamp::MAX.epoch_secs() + 1),
            None
        );
        assert_eq!("9999-12-31T23:59:59Z".parse(), Ok(Timestamp::MAX));
    }

    #[test]
    fn displays_known_epochs() {
        // Edge cases: epoch 0, leap day, year boundary, and a post-2038 value
        // (no 32-bit rollover).
        assert_eq!(at(0).to_string(), "1970-01-01T00:00:00Z");
        assert_eq!(at(1_709_164_800).to_string(), "2024-02-29T00:00:00Z");
        assert_eq!(at(1_767_225_599).to_string(), "2025-12-31T23:59:59Z");
        assert_eq!(at(1_767_225_600).to_string(), "2026-01-01T00:00:00Z");
        assert_eq!(at(4_102_444_800).to_string(), "2100-01-01T00:00:00Z");
    }

    #[test]
    fn display_round_trips_through_from_str() {
        for epoch in [
            0,
            1,
            86_399,
            86_400,
            951_782_400,   // 2000-02-29 (divisible-by-400 leap year)
            1_709_164_800, // 2024-02-29
            1_740_000_000,
            1_767_225_599,
            2_147_483_648, // past the 32-bit signed rollover
            4_102_444_800, // 2100
        ] {
            let formatted = at(epoch).to_string();
            assert_eq!(
                formatted.parse(),
                Ok(at(epoch)),
                "round-trip failed for {epoch} ({formatted})"
            );
        }
    }

    #[test]
    fn from_str_rejects_malformed_input() {
        for bad in [
            "",
            "1740000000",
            "2026-06-10",
            "2026-06-10T14:42:17",       // missing Z
            "2026-06-10T14:42:17+02:00", // non-UTC offset
            "2026-13-01T00:00:00Z",      // month 13
            "2026-02-29T00:00:00Z",      // not a leap year
            "2100-02-29T00:00:00Z",      // century, not a leap year
            "2026-04-31T00:00:00Z",      // April has 30 days
            "2026-06-00T00:00:00Z",      // day 0
            "2026-06-10T24:00:00Z",      // hour 24
            "2026-06-10T23:59:60Z",      // leap second
            "2026-+6-10T14:42:17Z",      // sign inside a field
            "1969-12-31T23:59:59Z",      // pre-epoch
            "2026-06-10T14:42:éZ",       // non-ASCII across digit slots
            "not a timestamp at allZ",
        ] {
            assert_eq!(
                bad.parse::<Timestamp>(),
                Err(ParseTimestampError {
                    input: bad.to_string()
                }),
                "must reject {bad:?}"
            );
        }
    }

    #[test]
    fn from_str_accepts_leap_days() {
        assert_eq!("2000-02-29T00:00:00Z".parse(), Ok(at(951_782_400)));
        assert_eq!("2024-02-29T00:00:00Z".parse(), Ok(at(1_709_164_800)));
    }

    #[test]
    fn parse_error_names_the_input() {
        let message = "yesterday".parse::<Timestamp>().unwrap_err().to_string();
        assert!(message.contains("\"yesterday\""), "{message}");
    }

    #[test]
    fn serializes_as_the_rfc3339_string() {
        assert_eq!(
            serde_json::to_string(&at(1_740_000_000)).expect("serialize"),
            r#""2025-02-19T21:20:00Z""#
        );
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn deserializes_only_the_rfc3339_string() {
        let decoded: Timestamp =
            serde_json::from_str(r#""2025-02-19T21:20:00Z""#).expect("deserialize");
        assert_eq!(decoded, at(1_740_000_000));
        assert!(serde_json::from_str::<Timestamp>("1740000000").is_err());
        assert!(serde_json::from_str::<Timestamp>(r#""yesterday""#).is_err());
    }
}
