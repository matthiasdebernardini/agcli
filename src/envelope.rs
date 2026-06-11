use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Serialize, Serializer};
use serde_json::Value;

/// Latest epoch second representable as a four-digit-year RFC 3339 timestamp
/// (9999-12-31T23:59:59Z). Larger pins would emit a string the crate's own
/// [`parse_rfc3339_utc`] rejects, breaking round-trips of our own output.
const MAX_RFC3339_EPOCH: u64 = 253_402_300_799;

fn epoch_secs() -> u64 {
    // SOURCE_DATE_EPOCH (the reproducible-builds convention) pins the envelope
    // timestamp so two runs of the same command can be byte-compared. Per the
    // spec a malformed value must not fall back to the wall clock — that would
    // silently defeat the pin — so it clamps to 0; negative values clamp to 0;
    // values past the four-digit-year ceiling clamp to MAX_RFC3339_EPOCH.
    if let Ok(raw) = std::env::var("SOURCE_DATE_EPOCH") {
        return pin_from_source_date_epoch(&raw);
    }
    // Never panic on a misconfigured clock: a timestamp of 0 keeps the
    // envelope valid and JSON-emittable, honoring the "JSON always" contract
    // even on a machine whose wall clock is set before 1970.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resolve a `SOURCE_DATE_EPOCH` value to a usable epoch second. Malformed
/// and negative values clamp to 0 (never the wall clock — that would silently
/// defeat the pin); oversized values clamp to [`MAX_RFC3339_EPOCH`].
fn pin_from_source_date_epoch(raw: &str) -> u64 {
    match raw.trim().parse::<i128>() {
        Ok(n) if n < 0 => 0,
        Ok(n) => u64::try_from(n)
            .unwrap_or(MAX_RFC3339_EPOCH)
            .min(MAX_RFC3339_EPOCH),
        Err(_) => 0,
    }
}

/// Format epoch seconds as an RFC 3339 UTC timestamp (`2026-06-10T14:42:17Z`).
///
/// Hand-rolled (Howard Hinnant's civil-from-days algorithm) so the dependency
/// set stays serde/serde_json/tokio. Envelopes serialize `timestamp` through
/// this so agents read the current date directly instead of epoch seconds.
pub(crate) fn rfc3339_utc(epoch: u64) -> String {
    let secs_of_day = epoch % 86_400;
    let (hh, mm, ss) = (
        secs_of_day / 3_600,
        secs_of_day % 3_600 / 60,
        secs_of_day % 60,
    );

    // epoch is unsigned, so plain division suffices: no pre-1970 date can
    // reach here (epoch_secs clamps a misconfigured clock to 0).
    let z = epoch / 86_400 + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);

    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Parse exactly the shape [`rfc3339_utc`] emits (`YYYY-MM-DDTHH:MM:SSZ`)
/// back to epoch seconds. Returns `None` for anything else, including
/// pre-1970 dates and non-UTC offsets.
#[cfg(any(feature = "deserialize", test))]
pub(crate) fn parse_rfc3339_utc(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| s[range].parse::<u64>().ok();
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hh > 23 || mm > 59 || ss > 59 {
        return None;
    }

    // Hinnant's days_from_civil, the inverse of rfc3339_utc above.
    let y = year - u64::from(month <= 2);
    let era = y / 400;
    let yoe = y % 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3_600 + mm * 60 + ss)
}

/// Accept `timestamp` as either the RFC 3339 string current envelopes emit or
/// the legacy epoch integer (pre-0.11 captures). Unparseable values fold to 0
/// — the same lenient default as a missing field.
#[cfg(feature = "deserialize")]
pub(crate) fn deserialize_timestamp<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Epoch(u64),
        Rfc3339(String),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Repr::deserialize(deserializer)? {
        Repr::Epoch(n) => n,
        Repr::Rfc3339(s) => parse_rfc3339_utc(&s).unwrap_or(0),
        Repr::Other(_) => 0,
    })
}

/// Mask an exit code into the 0–255 range a Unix process status occupies.
///
/// `std::process::exit` truncates to the low 8 bits, so an out-of-range code
/// would make the serialized `exit_code` field disagree with the real process
/// status (`exit(256)` → status `0`). Masking keeps the JSON field equal to
/// what the shell observes via `$?`.
///
/// This masks silently: it runs in the framework's post-handler path, *after*
/// the handler panic guard, so an assertion here would unwind past the
/// envelope machinery and print nothing to stdout — the exact non-JSON
/// failure the framework exists to prevent. The development-time
/// `debug_assert` for out-of-range codes lives in
/// [`crate::CommandOutput::exit_code`] / [`crate::CommandError::exit_code`]
/// instead, where a panic is caught and folded into a `HANDLER_PANIC`
/// envelope.
pub(crate) fn normalize_exit_code(code: i32) -> i32 {
    code & 0xff
}

/// Typed process exit codes for agent self-correction.
///
/// Agents (and shells) branch on the integer exit status without parsing
/// error text. The taxonomy mirrors the conventions popularized by
/// agent-native CLI generators: a distinct code per failure class so a
/// caller can decide whether to retry, re-authenticate, or fix arguments.
///
/// Each envelope carries the code in two places that always agree: the
/// serialized `exit_code` JSON field and the process status returned by
/// [`crate::Execution::exit_code`]. Use these constants with
/// [`crate::CommandError::exit_code`] and [`crate::CommandOutput::exit_code`]
/// — they all fall inside the maskable 0–255 range.
pub struct ExitCode;

impl ExitCode {
    /// Command succeeded.
    pub const SUCCESS: i32 = 0;
    /// Generic, unclassified failure (the default for handler errors).
    pub const ERROR: i32 = 1;
    /// Bad invocation: unknown command, bad flag, missing argument.
    pub const USAGE: i32 = 2;
    /// A requested resource does not exist.
    pub const NOT_FOUND: i32 = 3;
    /// Authentication or authorization failure.
    pub const AUTH: i32 = 4;
    /// An upstream/API call failed.
    pub const API: i32 = 5;
    /// The caller is being rate limited and should back off.
    pub const RATE_LIMITED: i32 = 7;
}

/// HATEOAS action template that tells an agent what to run next.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
pub struct NextAction {
    pub command: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<BTreeMap<String, ActionParam>>,
}

impl NextAction {
    pub fn new(command: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            description: description.into(),
            params: None,
        }
    }

    pub fn with_param(mut self, name: impl Into<String>, param: ActionParam) -> Self {
        self.params
            .get_or_insert_with(BTreeMap::new)
            .insert(name.into(), param);
        self
    }
}

/// Metadata for a templated `next_action` parameter.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
pub struct ActionParam {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

impl ActionParam {
    pub fn new() -> Self {
        Self {
            description: None,
            value: None,
            default: None,
            enum_values: None,
            required: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn value(mut self, value: impl Into<Value>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn default(mut self, value: impl Into<Value>) -> Self {
        self.default = Some(value.into());
        self
    }

    pub fn enum_values<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.enum_values = Some(values.into_iter().map(Into::into).collect());
        self
    }

    pub fn required(mut self, required: bool) -> Self {
        self.required = Some(required);
        self
    }
}

impl Default for ActionParam {
    fn default() -> Self {
        Self::new()
    }
}

/// Machine-readable error payload.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
pub struct ErrorBody {
    pub message: String,
    pub code: String,
    pub retryable: bool,
}

impl ErrorBody {
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            retryable: false,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

/// Success response envelope.
///
/// Always serializes with `"ok": true`. The `ok` field is not stored;
/// it is injected at serialization time and validated at deserialization time.
#[derive(Clone, Debug, PartialEq)]
pub struct SuccessEnvelope {
    pub command: String,
    pub timestamp: u64,
    pub schema_version: Option<String>,
    pub result: Value,
    pub next_actions: Vec<NextAction>,
    /// Process exit code. Defaults to [`ExitCode::SUCCESS`]. A handler may
    /// override it (e.g. a `doctor` command reporting an unhealthy system
    /// while still producing a valid `ok: true` envelope). Serialized as the
    /// `exit_code` field and masked to 0–255 so it equals the process status.
    pub exit_code: i32,
}

impl SuccessEnvelope {
    pub fn new(command: impl Into<String>, result: Value, next_actions: Vec<NextAction>) -> Self {
        Self {
            command: command.into(),
            timestamp: epoch_secs(),
            schema_version: None,
            result,
            next_actions,
            exit_code: ExitCode::SUCCESS,
        }
    }

    pub fn schema_version(mut self, version: impl Into<String>) -> Self {
        self.schema_version = Some(version.into());
        self
    }

    pub fn exit_code(mut self, code: i32) -> Self {
        self.exit_code = normalize_exit_code(code);
        self
    }
}

impl Serialize for SuccessEnvelope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let field_count = 6 + usize::from(self.schema_version.is_some());
        let mut s = serializer.serialize_struct("SuccessEnvelope", field_count)?;
        s.serialize_field("ok", &true)?;
        s.serialize_field("command", &self.command)?;
        s.serialize_field("timestamp", &rfc3339_utc(self.timestamp))?;
        s.serialize_field("exit_code", &self.exit_code)?;
        if let Some(ref sv) = self.schema_version {
            s.serialize_field("schema_version", sv)?;
        }
        s.serialize_field("result", &self.result)?;
        s.serialize_field("next_actions", &self.next_actions)?;
        s.end()
    }
}

#[cfg(feature = "deserialize")]
impl<'de> serde::Deserialize<'de> for SuccessEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Raw {
            ok: bool,
            command: String,
            #[serde(default, deserialize_with = "deserialize_timestamp")]
            timestamp: u64,
            #[serde(default)]
            exit_code: i32,
            #[serde(default)]
            schema_version: Option<String>,
            result: Value,
            next_actions: Vec<NextAction>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if !raw.ok {
            return Err(serde::de::Error::custom(
                "expected ok=true for SuccessEnvelope",
            ));
        }
        Ok(SuccessEnvelope {
            command: raw.command,
            timestamp: raw.timestamp,
            schema_version: raw.schema_version,
            result: raw.result,
            next_actions: raw.next_actions,
            exit_code: raw.exit_code,
        })
    }
}

/// Error response envelope.
///
/// Always serializes with `"ok": false`. The `ok` field is not stored;
/// it is injected at serialization time and validated at deserialization time.
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorEnvelope {
    pub command: String,
    pub timestamp: u64,
    pub schema_version: Option<String>,
    pub error: ErrorBody,
    pub fix: String,
    pub next_actions: Vec<NextAction>,
    /// Process exit code. Defaults to [`ExitCode::ERROR`]. Serialized as the
    /// `exit_code` field (alongside the failure class in `error.code`) and
    /// masked to 0–255 so it equals the process status.
    pub exit_code: i32,
}

impl ErrorEnvelope {
    pub fn new(
        command: impl Into<String>,
        message: impl Into<String>,
        code: impl Into<String>,
        fix: impl Into<String>,
        next_actions: Vec<NextAction>,
    ) -> Self {
        Self {
            command: command.into(),
            timestamp: epoch_secs(),
            schema_version: None,
            error: ErrorBody::new(message, code),
            fix: fix.into(),
            next_actions,
            exit_code: ExitCode::ERROR,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.error.retryable = retryable;
        self
    }

    pub fn schema_version(mut self, version: impl Into<String>) -> Self {
        self.schema_version = Some(version.into());
        self
    }

    pub fn exit_code(mut self, code: i32) -> Self {
        self.exit_code = normalize_exit_code(code);
        self
    }
}

impl Serialize for ErrorEnvelope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let field_count = 7 + usize::from(self.schema_version.is_some());
        let mut s = serializer.serialize_struct("ErrorEnvelope", field_count)?;
        s.serialize_field("ok", &false)?;
        s.serialize_field("command", &self.command)?;
        s.serialize_field("timestamp", &rfc3339_utc(self.timestamp))?;
        s.serialize_field("exit_code", &self.exit_code)?;
        if let Some(ref sv) = self.schema_version {
            s.serialize_field("schema_version", sv)?;
        }
        s.serialize_field("error", &self.error)?;
        s.serialize_field("fix", &self.fix)?;
        s.serialize_field("next_actions", &self.next_actions)?;
        s.end()
    }
}

#[cfg(feature = "deserialize")]
impl<'de> serde::Deserialize<'de> for ErrorEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        fn default_error_exit_code() -> i32 {
            ExitCode::ERROR
        }
        #[derive(serde::Deserialize)]
        struct Raw {
            ok: bool,
            command: String,
            #[serde(default, deserialize_with = "deserialize_timestamp")]
            timestamp: u64,
            #[serde(default = "default_error_exit_code")]
            exit_code: i32,
            #[serde(default)]
            schema_version: Option<String>,
            error: ErrorBody,
            fix: String,
            next_actions: Vec<NextAction>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if raw.ok {
            return Err(serde::de::Error::custom(
                "expected ok=false for ErrorEnvelope",
            ));
        }
        Ok(ErrorEnvelope {
            command: raw.command,
            timestamp: raw.timestamp,
            schema_version: raw.schema_version,
            error: raw.error,
            fix: raw.fix,
            next_actions: raw.next_actions,
            exit_code: raw.exit_code,
        })
    }
}

/// Unified envelope enum.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Envelope {
    Success(SuccessEnvelope),
    Error(ErrorEnvelope),
}

#[cfg(feature = "deserialize")]
impl<'de> serde::Deserialize<'de> for Envelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let ok = value
            .get("ok")
            .and_then(Value::as_bool)
            .ok_or_else(|| serde::de::Error::custom("missing or invalid `ok` field"))?;
        if ok {
            serde_json::from_value(value)
                .map(Envelope::Success)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Envelope::Error)
                .map_err(serde::de::Error::custom)
        }
    }
}

impl Envelope {
    pub fn command(&self) -> &str {
        match self {
            Self::Success(value) => &value.command,
            Self::Error(value) => &value.command,
        }
    }

    pub fn ok(&self) -> bool {
        matches!(self, Self::Success(_))
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Success(value) => value.exit_code,
            Self::Error(value) => value.exit_code,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            serde_json::to_string(&self.serialization_fallback(e))
                .expect("error-shaped fallback envelope always serializes")
        })
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|e| {
            serde_json::to_string_pretty(&self.serialization_fallback(e))
                .expect("error-shaped fallback envelope always serializes")
        })
    }

    /// Build a real, shape-consistent error envelope for the (effectively
    /// unreachable) case where serializing `self` fails. An agent that always
    /// parses `error.code` / `error.retryable` still finds them here, instead
    /// of a bare `error` string with no `exit_code`. Serializing this fallback
    /// cannot itself fail (no non-string object keys, no NaN/Inf numbers).
    fn serialization_fallback(&self, err: serde_json::Error) -> ErrorEnvelope {
        ErrorEnvelope::new(
            self.command().to_string(),
            format!("serialization failed: {err}"),
            "SERIALIZATION_FAILED",
            "Report this as a bug; the command produced a value that could not be serialized.",
            Vec::new(),
        )
    }
}

impl From<SuccessEnvelope> for Envelope {
    fn from(value: SuccessEnvelope) -> Self {
        Self::Success(value)
    }
}

impl From<ErrorEnvelope> for Envelope {
    fn from(value: ErrorEnvelope) -> Self {
        Self::Error(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_date_epoch_pin_parses_and_clamps() {
        // Normal value passes through.
        assert_eq!(pin_from_source_date_epoch("1700000000"), 1_700_000_000);
        assert_eq!(pin_from_source_date_epoch(" 0 "), 0);
        // Malformed and negative values clamp to 0 per the
        // reproducible-builds convention — never the wall clock.
        assert_eq!(pin_from_source_date_epoch("not-a-number"), 0);
        assert_eq!(pin_from_source_date_epoch(""), 0);
        assert_eq!(pin_from_source_date_epoch("-5"), 0);
        // Values past the four-digit-year ceiling clamp so the emitted
        // timestamp stays parseable by our own parser.
        assert_eq!(
            pin_from_source_date_epoch("18446744073709551615"),
            MAX_RFC3339_EPOCH
        );
        assert_eq!(
            pin_from_source_date_epoch("999999999999999999999999"),
            MAX_RFC3339_EPOCH
        );
    }

    #[test]
    fn max_rfc3339_epoch_round_trips_through_own_parser() {
        let rendered = rfc3339_utc(MAX_RFC3339_EPOCH);
        assert_eq!(rendered, "9999-12-31T23:59:59Z");
        assert_eq!(parse_rfc3339_utc(&rendered), Some(MAX_RFC3339_EPOCH));
    }

    #[test]
    fn success_envelope_serializes_required_fields() {
        let envelope = SuccessEnvelope::new(
            "wokhei status",
            json!({ "healthy": true }),
            vec![NextAction::new("wokhei status", "Re-check health")],
        );

        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["ok"], Value::Bool(true));
        assert_eq!(
            encoded["command"],
            Value::String("wokhei status".to_string())
        );
        let ts = encoded["timestamp"]
            .as_str()
            .expect("timestamp is a string");
        assert!(
            parse_rfc3339_utc(ts).is_some(),
            "timestamp must be RFC 3339 UTC: {ts}"
        );
        assert!(encoded["next_actions"].is_array());
        // exit_code is always present; success defaults to 0.
        assert_eq!(encoded["exit_code"], json!(0));
        // schema_version omitted when None
        assert!(encoded.get("schema_version").is_none());
    }

    #[test]
    fn success_envelope_serializes_overridden_exit_code() {
        let envelope = SuccessEnvelope::new("cmd", json!(null), vec![]).exit_code(4);
        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["ok"], Value::Bool(true));
        assert_eq!(encoded["exit_code"], json!(4));
    }

    #[test]
    fn success_envelope_with_schema_version() {
        let envelope = SuccessEnvelope::new("cmd", json!(null), vec![]).schema_version("test.v1");
        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["schema_version"], json!("test.v1"));
    }

    #[test]
    fn error_envelope_serializes_fix() {
        let envelope = ErrorEnvelope::new(
            "wokhei publish",
            "invalid json",
            "INVALID_JSON",
            "Use valid JSON input",
            vec![],
        );

        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["ok"], Value::Bool(false));
        assert_eq!(
            encoded["error"]["code"],
            Value::String("INVALID_JSON".to_string())
        );
        assert_eq!(encoded["error"]["retryable"], Value::Bool(false));
        let ts = encoded["timestamp"]
            .as_str()
            .expect("timestamp is a string");
        assert!(
            parse_rfc3339_utc(ts).is_some(),
            "timestamp must be RFC 3339 UTC: {ts}"
        );
        assert_eq!(
            encoded["fix"],
            Value::String("Use valid JSON input".to_string())
        );
        // exit_code is always present; errors default to 1.
        assert_eq!(encoded["exit_code"], json!(1));
        // schema_version omitted when None
        assert!(encoded.get("schema_version").is_none());
    }

    #[test]
    fn error_envelope_serializes_typed_exit_code() {
        let envelope =
            ErrorEnvelope::new("cmd", "missing", "NOT_FOUND", "check id", vec![]).exit_code(3);
        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["exit_code"], json!(3));
    }

    #[test]
    fn exit_code_in_range_matches_json_and_status() {
        // The serialized exit_code field must equal what the process status
        // will be (masked to the low 8 bits).
        let e = ErrorEnvelope::new("cmd", "m", "C", "fix", vec![]).exit_code(7);
        let encoded = serde_json::to_value(&e).expect("serialize");
        assert_eq!(encoded["exit_code"], json!(7));
        assert_eq!(e.exit_code, 7 & 0xff);
    }

    #[test]
    fn exit_code_masked_to_process_status() {
        // 261 (256 + 5) truncates to 5 under std::process::exit; the JSON field
        // must agree. This setter runs in the framework's post-handler path
        // (outside the handler panic guard), so it masks silently in every
        // build — the development-time assert lives on the handler-side
        // setters (`CommandOutput::exit_code` / `CommandError::exit_code`).
        let e = SuccessEnvelope::new("cmd", json!(null), vec![]).exit_code(256 + 5);
        assert_eq!(e.exit_code, 5);
        let encoded = serde_json::to_value(&e).expect("serialize");
        assert_eq!(encoded["exit_code"], json!(5));
    }

    #[test]
    fn error_envelope_retryable() {
        let envelope = ErrorEnvelope::new("cmd", "err", "E", "fix", vec![])
            .retryable(true)
            .schema_version("test.v1");
        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["error"]["retryable"], Value::Bool(true));
        assert_eq!(encoded["schema_version"], json!("test.v1"));
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn success_envelope_roundtrips() {
        let original = SuccessEnvelope::new(
            "test cmd",
            json!({ "data": 42 }),
            vec![NextAction::new("test cmd", "Run again")],
        )
        .schema_version("test.v1")
        .exit_code(2);
        let json = serde_json::to_string(&original).expect("must serialize");
        let decoded: SuccessEnvelope = serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(original, decoded);
        assert_eq!(decoded.exit_code, 2);
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn error_envelope_roundtrips() {
        let original = ErrorEnvelope::new("test cmd", "oops", "ERR", "fix it", vec![])
            .retryable(true)
            .schema_version("test.v1")
            .exit_code(7);
        let json = serde_json::to_string(&original).expect("must serialize");
        let decoded: ErrorEnvelope = serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(original, decoded);
        assert_eq!(decoded.exit_code, 7);
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn success_envelope_rejects_ok_false() {
        let json = r#"{"ok":false,"command":"test","timestamp":0,"result":null,"next_actions":[]}"#;
        let result: Result<SuccessEnvelope, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn error_envelope_rejects_ok_true() {
        let json = r#"{"ok":true,"command":"test","timestamp":0,"error":{"message":"x","code":"X","retryable":false},"fix":"y","next_actions":[]}"#;
        let result: Result<ErrorEnvelope, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn rfc3339_utc_formats_known_epochs() {
        // Edge cases: epoch 0 (misconfigured-clock fallback), leap day,
        // year boundary, and a post-2038 value (no 32-bit rollover).
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_767_225_599), "2025-12-31T23:59:59Z");
        assert_eq!(rfc3339_utc(1_767_225_600), "2026-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(4_102_444_800), "2100-01-01T00:00:00Z");
    }

    #[test]
    fn rfc3339_utc_roundtrips_through_parser() {
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
            let formatted = rfc3339_utc(epoch);
            assert_eq!(
                parse_rfc3339_utc(&formatted),
                Some(epoch),
                "round-trip failed for {epoch} ({formatted})"
            );
        }
    }

    #[test]
    fn parse_rfc3339_utc_rejects_malformed_input() {
        for bad in [
            "",
            "1740000000",
            "2026-06-10",
            "2026-06-10T14:42:17",       // missing Z
            "2026-06-10T14:42:17+02:00", // non-UTC offset
            "2026-13-01T00:00:00Z",      // month 13
            "2026-06-10T24:00:00Z",      // hour 24
            "1969-12-31T23:59:59Z",      // pre-epoch
            "not a timestamp at allZ",
        ] {
            assert_eq!(parse_rfc3339_utc(bad), None, "must reject {bad:?}");
        }
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn envelope_accepts_legacy_epoch_timestamp() {
        // Pre-0.11 captures carry timestamp as epoch seconds.
        let json =
            r#"{"ok":true,"command":"cmd","timestamp":1740000000,"result":null,"next_actions":[]}"#;
        let decoded: SuccessEnvelope = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(decoded.timestamp, 1_740_000_000);

        let json = r#"{"ok":false,"command":"cmd","timestamp":"2025-02-19T21:20:00Z","error":{"message":"x","code":"X","retryable":false},"fix":"y","next_actions":[]}"#;
        let decoded: ErrorEnvelope = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(decoded.timestamp, 1_740_000_000);
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn envelope_dispatches_by_ok_field() {
        let success_json =
            serde_json::to_string(&SuccessEnvelope::new("cmd", json!(null), vec![])).unwrap();
        let error_json =
            serde_json::to_string(&ErrorEnvelope::new("cmd", "err", "E", "fix", vec![])).unwrap();

        let s: Envelope = serde_json::from_str(&success_json).expect("must deserialize success");
        assert!(matches!(s, Envelope::Success(_)));

        let e: Envelope = serde_json::from_str(&error_json).expect("must deserialize error");
        assert!(matches!(e, Envelope::Error(_)));
    }
}
