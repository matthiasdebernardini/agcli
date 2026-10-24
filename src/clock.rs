//! The run's clock: where every envelope and stream event of a run reads its
//! timestamp from, at the moment it is written.

use std::env;
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::timestamp::Timestamp;

/// A run's clock: one set with [`AgentCli::clock`](crate::AgentCli::clock),
/// wrapped by [`checked`], or the default one.
pub(crate) type ClockFn = dyn Fn() -> Timestamp + Send + Sync;

/// The variable that pins the default clock, per the reproducible-builds
/// convention.
const SOURCE_DATE_EPOCH: &str = "SOURCE_DATE_EPOCH";

/// `SOURCE_DATE_EPOCH` is set to something that is not a Unix timestamp in
/// seconds. Holds the offending value, lossily decoded for the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InvalidSourceDateEpoch(pub(crate) String);

impl fmt::Display for InvalidSourceDateEpoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{SOURCE_DATE_EPOCH} is not a Unix timestamp in seconds \
             (0 to {}): {:?}",
            Timestamp::MAX.epoch_secs(),
            self.0
        )
    }
}

/// The default clock: pinned to `SOURCE_DATE_EPOCH` when it is set, else the
/// system clock. Read once per run, before anything else, so a malformed pin
/// fails the run before any command runs; every later reading of the
/// returned clock succeeds.
///
/// # Errors
///
/// [`InvalidSourceDateEpoch`] when `SOURCE_DATE_EPOCH` is set but is not a
/// run of ASCII digits naming a second the RFC 3339 timestamp can carry. The
/// reproducible-builds spec requires an error there: falling back to the
/// wall clock would silently defeat the pin.
pub(crate) fn default_clock() -> Result<Arc<ClockFn>, InvalidSourceDateEpoch> {
    match env::var_os(SOURCE_DATE_EPOCH) {
        Some(raw) => {
            let pinned = parse_source_date_epoch(&raw.to_string_lossy())?;
            Ok(Arc::new(move || pinned))
        }
        None => Ok(Arc::new(system_now)),
    }
}

/// Parse a `SOURCE_DATE_EPOCH` value: ASCII digits only (no sign, no
/// whitespace), at most [`Timestamp::MAX`]. A non-UTF-8 value reaches here
/// with replacement characters, so it fails too.
fn parse_source_date_epoch(raw: &str) -> Result<Timestamp, InvalidSourceDateEpoch> {
    let invalid = || InvalidSourceDateEpoch(raw.to_string());
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    raw.parse::<u64>()
        .ok()
        .and_then(Timestamp::from_epoch_secs)
        .ok_or_else(invalid)
}

/// The system clock.
///
/// A system clock set before 1970 reads as [`Timestamp::UNIX_EPOCH`]: it is
/// the earliest second an envelope can carry, and the envelope that would
/// report the misconfigured clock needs a timestamp too.
///
/// # Panics
///
/// When the system clock reads past [`Timestamp::MAX`], as [`checked`] does.
pub(crate) fn system_now() -> Timestamp {
    checked(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
    )
}

/// The timestamp a clock reading of `secs` names.
///
/// # Panics
///
/// When `secs` is past [`Timestamp::MAX`]: no RFC 3339 timestamp can name it,
/// and writing a wrong one would be a silent fallback.
pub(crate) fn checked(secs: u64) -> Timestamp {
    Timestamp::from_epoch_secs(secs).unwrap_or_else(|| {
        panic!(
            "the clock read {secs} seconds, past Timestamp::MAX ({} = {})",
            Timestamp::MAX.epoch_secs(),
            Timestamp::MAX
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_date_epoch_accepts_plain_seconds() {
        assert_eq!(parse_source_date_epoch("0"), Ok(Timestamp::UNIX_EPOCH));
        assert_eq!(
            parse_source_date_epoch("1700000000"),
            Ok(checked(1_700_000_000))
        );
        assert_eq!(parse_source_date_epoch("253402300799"), Ok(Timestamp::MAX));
    }

    #[test]
    #[should_panic(expected = "past Timestamp::MAX (253402300799 = 9999-12-31T23:59:59Z)")]
    fn checked_panics_past_the_last_timestamp() {
        let _ = checked(253_402_300_800);
    }

    #[test]
    fn source_date_epoch_rejects_anything_else() {
        for bad in [
            "",
            "not-a-number",
            "-5",
            "+5",
            " 0 ",
            "1.5",
            "253402300800",
            "18446744073709551616",
        ] {
            assert_eq!(
                parse_source_date_epoch(bad),
                Err(InvalidSourceDateEpoch(bad.to_string())),
                "must reject {bad:?}"
            );
        }
    }

    #[test]
    fn invalid_source_date_epoch_names_the_value() {
        let message = InvalidSourceDateEpoch("-5".to_string()).to_string();
        assert!(message.contains("SOURCE_DATE_EPOCH"), "{message}");
        assert!(message.contains("\"-5\""), "{message}");
    }
}
