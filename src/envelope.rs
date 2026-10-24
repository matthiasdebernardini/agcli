//! The JSON envelopes every command answers with.

#[cfg(feature = "deserialize")]
use serde::de;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::codes::{ErrorCode, ExitCode};
use crate::command_error::CommandError;
use crate::next_action::NextAction;
use crate::timestamp::Timestamp;

/// Machine-readable error payload: the envelope's `error` object, with its
/// keys in this order.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[non_exhaustive]
pub struct ErrorBody {
    /// What went wrong, for the agent to read.
    pub message: String,
    /// The failure class, for the agent to branch on.
    pub code: ErrorCode,
    /// Whether the same invocation can succeed later.
    pub retryable: bool,
}

/// Success response envelope.
///
/// Always serializes with `"ok": true`. The `ok` field is not stored;
/// it is injected at serialization time and validated at deserialization time.
///
/// The framework builds every envelope; read one from
/// [`Execution::envelope`](crate::Execution::envelope), or decode one with
/// the `deserialize` feature.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct SuccessEnvelope {
    /// The invocation as one shell line that reruns as printed: the program's
    /// file name, then every argument, single-quoted where it needs to be.
    pub command: String,
    /// The run's clock, read when the envelope was built. Serialized as the
    /// `timestamp` key, an RFC 3339 UTC string.
    pub timestamp: Timestamp,
    /// The CLI's [`AgentCli::schema_version`](crate::AgentCli::schema_version),
    /// when it set one.
    pub schema_version: Option<String>,
    /// The command's payload, after `--select` and `--compact`.
    pub result: Value,
    /// Set when the reserved `--select` projection could not do what it was
    /// asked (no field names, or names that match nothing): what went wrong
    /// and which fields exist. Serialized as the `select_warning` key, beside
    /// `result` rather than inside it, and omitted when `None`.
    pub select_warning: Option<String>,
    /// What the agent can run next. Empty under `--quiet`.
    pub next_actions: Vec<NextAction>,
    /// Process exit code. Defaults to [`ExitCode::SUCCESS`]. A handler may
    /// override it (e.g. a `doctor` command reporting an unhealthy system
    /// while still producing a valid `ok: true` envelope). Serialized as the
    /// `exit_code` field, equal to the process status.
    pub exit_code: ExitCode,
}

impl SuccessEnvelope {
    pub(crate) fn new(
        command: String,
        timestamp: Timestamp,
        result: Value,
        next_actions: Vec<NextAction>,
    ) -> Self {
        Self {
            command,
            timestamp,
            schema_version: None,
            result,
            select_warning: None,
            next_actions,
            exit_code: ExitCode::SUCCESS,
        }
    }

    #[must_use]
    pub(crate) fn select_warning(mut self, warning: Option<String>) -> Self {
        self.select_warning = warning;
        self
    }

    #[must_use]
    pub(crate) fn schema_version(mut self, version: &str) -> Self {
        self.schema_version = Some(version.to_string());
        self
    }

    #[must_use]
    pub(crate) fn exit_code(mut self, code: ExitCode) -> Self {
        self.exit_code = code;
        self
    }
}

impl Serialize for SuccessEnvelope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.serialize_framed(serializer, None)
    }
}

impl SuccessEnvelope {
    /// Serialize the envelope, led by a `type` key when it is a stream's
    /// terminal line.
    fn serialize_framed<S: Serializer>(
        &self,
        serializer: S,
        stream_type: Option<&str>,
    ) -> Result<S::Ok, S::Error> {
        let field_count = 6
            + usize::from(stream_type.is_some())
            + usize::from(self.schema_version.is_some())
            + usize::from(self.select_warning.is_some());
        let mut s = serializer.serialize_map(Some(field_count))?;
        if let Some(kind) = stream_type {
            s.serialize_entry("type", kind)?;
        }
        s.serialize_entry("ok", &true)?;
        s.serialize_entry("command", &self.command)?;
        s.serialize_entry("timestamp", &self.timestamp)?;
        s.serialize_entry("exit_code", &self.exit_code)?;
        if let Some(ref version) = self.schema_version {
            s.serialize_entry("schema_version", version)?;
        }
        s.serialize_entry("result", &self.result)?;
        if let Some(ref warning) = self.select_warning {
            s.serialize_entry("select_warning", warning)?;
        }
        s.serialize_entry("next_actions", &self.next_actions)?;
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
            timestamp: Timestamp,
            exit_code: ExitCode,
            schema_version: Option<String>,
            result: Value,
            select_warning: Option<String>,
            next_actions: Vec<NextAction>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if !raw.ok {
            return Err(de::Error::custom("expected ok=true for SuccessEnvelope"));
        }
        Ok(SuccessEnvelope {
            command: raw.command,
            timestamp: raw.timestamp,
            schema_version: raw.schema_version,
            result: raw.result,
            select_warning: raw.select_warning,
            next_actions: raw.next_actions,
            exit_code: raw.exit_code,
        })
    }
}

/// Error response envelope.
///
/// Always serializes with `"ok": false`. The `ok` field is not stored;
/// it is injected at serialization time and validated at deserialization time.
///
/// Built by the framework from a [`CommandError`]; read one from
/// [`Execution::envelope`](crate::Execution::envelope), or decode one with
/// the `deserialize` feature.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ErrorEnvelope {
    /// The invocation as one shell line that reruns as printed, as in
    /// [`SuccessEnvelope::command`].
    pub command: String,
    /// The run's clock, read when the envelope was built. Serialized as the
    /// `timestamp` key, an RFC 3339 UTC string.
    pub timestamp: Timestamp,
    /// The CLI's [`AgentCli::schema_version`](crate::AgentCli::schema_version),
    /// when it set one.
    pub schema_version: Option<String>,
    /// What went wrong, its class, and whether a retry can work.
    pub error: ErrorBody,
    /// How to recover.
    pub fix: String,
    /// What the agent can run to recover. `--quiet` leaves them in: they are
    /// the way out.
    pub next_actions: Vec<NextAction>,
    /// Process exit code: the error's own, else [`ExitCode::ERROR`].
    /// Serialized as the `exit_code` field (alongside the failure class in
    /// `error.code`), equal to the process status.
    pub exit_code: ExitCode,
    /// Optional structured payload describing the failure: the rejected
    /// rows, the upstream reply, the conflicting id. Serialized as the `data`
    /// key, between `fix` and `next_actions`, and left out when `None`.
    pub data: Option<Value>,
}

impl ErrorEnvelope {
    /// The envelope reporting `error` for the run of `command` at
    /// `timestamp`.
    pub(crate) fn new(command: String, timestamp: Timestamp, error: CommandError) -> Self {
        Self {
            command,
            timestamp,
            schema_version: None,
            error: ErrorBody {
                message: error.message,
                code: error.code,
                retryable: error.retryable,
            },
            fix: error.fix,
            next_actions: error.next_actions,
            exit_code: error.exit_code.unwrap_or(ExitCode::ERROR),
            data: error.data.map(|payload| *payload),
        }
    }

    #[must_use]
    pub(crate) fn schema_version(mut self, version: &str) -> Self {
        self.schema_version = Some(version.to_string());
        self
    }
}

impl Serialize for ErrorEnvelope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.serialize_framed(serializer, None)
    }
}

impl ErrorEnvelope {
    /// Serialize the envelope, led by a `type` key when it is a stream's
    /// terminal line.
    fn serialize_framed<S: Serializer>(
        &self,
        serializer: S,
        stream_type: Option<&str>,
    ) -> Result<S::Ok, S::Error> {
        let field_count = 7
            + usize::from(stream_type.is_some())
            + usize::from(self.schema_version.is_some())
            + usize::from(self.data.is_some());
        let mut s = serializer.serialize_map(Some(field_count))?;
        if let Some(kind) = stream_type {
            s.serialize_entry("type", kind)?;
        }
        s.serialize_entry("ok", &false)?;
        s.serialize_entry("command", &self.command)?;
        s.serialize_entry("timestamp", &self.timestamp)?;
        s.serialize_entry("exit_code", &self.exit_code)?;
        if let Some(ref version) = self.schema_version {
            s.serialize_entry("schema_version", version)?;
        }
        s.serialize_entry("error", &self.error)?;
        s.serialize_entry("fix", &self.fix)?;
        if let Some(ref data) = self.data {
            s.serialize_entry("data", data)?;
        }
        s.serialize_entry("next_actions", &self.next_actions)?;
        s.end()
    }
}

#[cfg(feature = "deserialize")]
impl<'de> serde::Deserialize<'de> for ErrorEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Raw {
            ok: bool,
            command: String,
            timestamp: Timestamp,
            exit_code: ExitCode,
            schema_version: Option<String>,
            error: ErrorBody,
            fix: String,
            data: Option<Value>,
            next_actions: Vec<NextAction>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if raw.ok {
            return Err(de::Error::custom("expected ok=false for ErrorEnvelope"));
        }
        Ok(ErrorEnvelope {
            command: raw.command,
            timestamp: raw.timestamp,
            schema_version: raw.schema_version,
            error: raw.error,
            fix: raw.fix,
            next_actions: raw.next_actions,
            exit_code: raw.exit_code,
            data: raw.data,
        })
    }
}

/// Either answer a run gives: a success or an error envelope.
///
/// Read it from [`Execution::envelope`](crate::Execution::envelope);
/// [`Envelope::ok`] tells which it is. It serializes as the envelope itself,
/// and decodes (with the `deserialize` feature) by its `ok` key.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Envelope {
    /// `"ok": true`: the command ran and answered.
    Success(SuccessEnvelope),
    /// `"ok": false`: the command, or the framework on its behalf, failed.
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
            .ok_or_else(|| de::Error::custom("missing or invalid `ok` field"))?;
        if ok {
            serde_json::from_value(value)
                .map(Envelope::Success)
                .map_err(de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Envelope::Error)
                .map_err(de::Error::custom)
        }
    }
}

impl Envelope {
    /// The envelope's `command`: the invocation as one shell line.
    #[must_use]
    pub fn command(&self) -> &str {
        match self {
            Self::Success(value) => &value.command,
            Self::Error(value) => &value.command,
        }
    }

    /// True for a success envelope, the value of its `ok` key.
    #[must_use]
    pub fn ok(&self) -> bool {
        matches!(self, Self::Success(_))
    }

    /// The envelope's `exit_code`, equal to the process status.
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Success(value) => value.exit_code,
            Self::Error(value) => value.exit_code,
        }
    }

    /// The envelope's timestamp.
    #[must_use]
    pub(crate) fn timestamp(&self) -> Timestamp {
        match self {
            Self::Success(value) => value.timestamp,
            Self::Error(value) => value.timestamp,
        }
    }

    /// Serialize the envelope to compact JSON. If `self` cannot serialize,
    /// this returns a `SERIALIZATION_FAILED` error envelope instead, so the
    /// caller always gets a JSON envelope.
    ///
    /// # Panics
    ///
    /// Only if that fallback error envelope itself fails to serialize, which
    /// cannot happen: it holds only strings, integers and booleans, with no
    /// non-string map keys and no non-finite floats.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            serde_json::to_string(&self.serialization_fallback(&e))
                .expect("error-shaped fallback envelope always serializes")
        })
    }

    /// Serialize the envelope to indented JSON, with the same fallback as
    /// [`Envelope::to_json`].
    ///
    /// # Panics
    ///
    /// Only if the fallback error envelope fails to serialize, which cannot
    /// happen (see [`Envelope::to_json`]).
    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|e| {
            serde_json::to_string_pretty(&self.serialization_fallback(&e))
                .expect("error-shaped fallback envelope always serializes")
        })
    }

    /// Serialize the envelope as a stream's terminal line: the same JSON as
    /// [`Envelope::to_json`] with `"type":"result"` or `"type":"error"` in
    /// front, and the same fallback.
    pub(crate) fn to_stream_json(&self) -> String {
        serde_json::to_string(&StreamTerminal(self)).unwrap_or_else(|e| {
            let fallback = Self::Error(self.serialization_fallback(&e));
            serde_json::to_string(&StreamTerminal(&fallback))
                .expect("error-shaped fallback envelope always serializes")
        })
    }

    /// [`Envelope::to_stream_json`], indented.
    pub(crate) fn to_stream_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&StreamTerminal(self)).unwrap_or_else(|e| {
            let fallback = Self::Error(self.serialization_fallback(&e));
            serde_json::to_string_pretty(&StreamTerminal(&fallback))
                .expect("error-shaped fallback envelope always serializes")
        })
    }

    /// Build a real, shape-consistent error envelope for the (effectively
    /// unreachable) case where serializing `self` fails. An agent that always
    /// parses `error.code` / `error.retryable` still finds them here, instead
    /// of a bare `error` string with no `exit_code`. Serializing this fallback
    /// cannot itself fail (no non-string object keys, no NaN/Inf numbers).
    fn serialization_fallback(&self, err: &serde_json::Error) -> ErrorEnvelope {
        ErrorEnvelope::new(
            self.command().to_string(),
            self.timestamp(),
            CommandError::new(
                ErrorCode::SERIALIZATION_FAILED,
                format!("serialization failed: {err}"),
                "Report this as a bug; the command produced a value that could not be serialized.",
            ),
        )
    }
}

/// An envelope framed as the terminal line of an NDJSON stream.
struct StreamTerminal<'envelope>(&'envelope Envelope);

impl Serialize for StreamTerminal<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Envelope::Success(envelope) => envelope.serialize_framed(serializer, Some("result")),
            Envelope::Error(envelope) => envelope.serialize_framed(serializer, Some("error")),
        }
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

    fn at(secs: u64) -> Timestamp {
        Timestamp::from_epoch_secs(secs).expect("in range")
    }

    fn success() -> SuccessEnvelope {
        SuccessEnvelope::new(
            "wokhei status".to_string(),
            at(1_740_000_000),
            json!({ "healthy": true }),
            vec![NextAction::new("wokhei status", "Re-check health")],
        )
    }

    fn error(code: ErrorCode) -> ErrorEnvelope {
        ErrorEnvelope::new(
            "wokhei publish".to_string(),
            at(1_740_000_000),
            CommandError::new(code, "invalid json", "Use valid JSON input"),
        )
    }

    #[test]
    fn success_envelope_serializes_required_fields() {
        let encoded = serde_json::to_value(success()).expect("must serialize");
        assert_eq!(encoded["ok"], Value::Bool(true));
        assert_eq!(encoded["command"], json!("wokhei status"));
        assert_eq!(encoded["timestamp"], json!("2025-02-19T21:20:00Z"));
        assert!(encoded["next_actions"].is_array());
        // exit_code is always present; success defaults to 0.
        assert_eq!(encoded["exit_code"], json!(0));
        // schema_version omitted when None
        assert!(encoded.get("schema_version").is_none());
    }

    #[test]
    fn success_envelope_serializes_overridden_exit_code() {
        let envelope = success().exit_code(ExitCode::AUTH);
        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["ok"], Value::Bool(true));
        assert_eq!(encoded["exit_code"], json!(4));
    }

    #[test]
    fn success_envelope_with_schema_version() {
        let envelope = success().schema_version("test.v1");
        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["schema_version"], json!("test.v1"));
    }

    #[test]
    fn error_envelope_serializes_fix() {
        let encoded =
            serde_json::to_value(error(ErrorCode::new("INVALID_JSON"))).expect("must serialize");
        assert_eq!(encoded["ok"], Value::Bool(false));
        assert_eq!(encoded["error"]["code"], json!("INVALID_JSON"));
        assert_eq!(encoded["error"]["retryable"], Value::Bool(false));
        assert_eq!(encoded["timestamp"], json!("2025-02-19T21:20:00Z"));
        assert_eq!(encoded["fix"], json!("Use valid JSON input"));
        // exit_code is always present; errors default to 1.
        assert_eq!(encoded["exit_code"], json!(1));
        // schema_version omitted when None
        assert!(encoded.get("schema_version").is_none());
    }

    #[test]
    fn error_envelope_carries_the_errors_exit_code_and_retry_hint() {
        let envelope = ErrorEnvelope::new(
            "cmd".to_string(),
            Timestamp::UNIX_EPOCH,
            CommandError::new(ErrorCode::new("NOT_FOUND"), "missing", "check id")
                .exit_code(ExitCode::NOT_FOUND)
                .retryable(true),
        )
        .schema_version("test.v1");
        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["exit_code"], json!(3));
        assert_eq!(encoded["error"]["retryable"], Value::Bool(true));
        assert_eq!(encoded["schema_version"], json!("test.v1"));
    }

    #[test]
    fn error_envelope_data_serializes_only_when_set() {
        let bare = serde_json::to_value(error(ErrorCode::new("C"))).expect("must serialize");
        assert!(
            bare.get("data").is_none(),
            "an unset payload must not add a key: {bare}"
        );

        let with_data = serde_json::to_value(ErrorEnvelope::new(
            "cmd".to_string(),
            Timestamp::UNIX_EPOCH,
            CommandError::new(ErrorCode::new("C"), "m", "fix").data(json!({ "rejected": [91] })),
        ))
        .expect("must serialize");
        assert_eq!(with_data["data"], json!({ "rejected": [91] }));
    }

    #[cfg(feature = "deserialize")]
    mod strict {
        use std::fmt;

        use serde::de::DeserializeOwned;

        use super::*;
        use crate::next_action::ActionParam;

        /// Serialize `value`, decode it back as `T`, and assert nothing changed.
        fn round_trip<T>(value: &T)
        where
            T: Serialize + DeserializeOwned + PartialEq + fmt::Debug,
        {
            let json = serde_json::to_string(value).expect("must serialize");
            let decoded: T = serde_json::from_str(&json).expect("must deserialize");
            assert_eq!(&decoded, value, "{json}");
        }

        /// `json` with the top-level `key` removed.
        fn without(json: &Value, key: &str) -> String {
            let mut value = json.clone();
            value.as_object_mut().expect("object").remove(key);
            value.to_string()
        }

        /// `json` with the top-level `key` set to `replacement`.
        fn with(json: &Value, key: &str, replacement: Value) -> String {
            let mut value = json.clone();
            value[key] = replacement;
            value.to_string()
        }

        fn full_param() -> ActionParam {
            ActionParam::new()
                .description("the id")
                .value(json!(7))
                .default(json!(1))
                .enum_values(["1", "7"])
                .required(true)
        }

        #[test]
        fn wire_types_round_trip() {
            round_trip(&ExitCode::RATE_LIMITED);
            round_trip(&ErrorCode::UNKNOWN_FLAG);
            round_trip(&full_param());
            round_trip(&ActionParam::new());
            round_trip(&NextAction::new("app get <id>", "Get one").with_param("id", full_param()));
            round_trip(&NextAction::new("app", "Inspect the tree"));
            round_trip(&error(ErrorCode::INVALID_ARG).error);
            round_trip(
                &success()
                    .schema_version("test.v1")
                    .exit_code(ExitCode::USAGE),
            );
            round_trip(&success());
            round_trip(&success().select_warning(Some("--select=x matched no fields".to_string())));
            let full = ErrorEnvelope::new(
                "test cmd".to_string(),
                at(1_740_000_000),
                CommandError::new(ErrorCode::new("ERR"), "oops", "fix it")
                    .retryable(true)
                    .exit_code(ExitCode::RATE_LIMITED)
                    .data(json!({ "rejected": [{ "line": 91 }] }))
                    .next_action(NextAction::new("test cmd", "Retry")),
            )
            .schema_version("test.v1");
            round_trip(&full);
            round_trip(&error(ErrorCode::PARSE_ERROR));
            round_trip(&Envelope::Success(success()));
            round_trip(&Envelope::Error(full));
            round_trip(&Timestamp::MAX);
        }

        #[test]
        fn envelope_dispatches_by_ok_field() {
            let success_json = serde_json::to_string(&success()).unwrap();
            let error_json = serde_json::to_string(&error(ErrorCode::new("E"))).unwrap();

            let s: Envelope = serde_json::from_str(&success_json).expect("success");
            assert!(matches!(s, Envelope::Success(_)));
            let e: Envelope = serde_json::from_str(&error_json).expect("error");
            assert!(matches!(e, Envelope::Error(_)));
        }

        #[test]
        fn success_envelope_rejects_a_missing_or_malformed_field() {
            let json = serde_json::to_value(success()).unwrap();
            let rejects = |text: String| {
                assert!(
                    serde_json::from_str::<SuccessEnvelope>(&text).is_err(),
                    "must reject {text}"
                );
            };
            for key in [
                "ok",
                "command",
                "timestamp",
                "exit_code",
                "result",
                "next_actions",
            ] {
                rejects(without(&json, key));
            }
            rejects(with(&json, "ok", json!(false)));
            // The epoch integer older captures carried is not a timestamp.
            rejects(with(&json, "timestamp", json!(1_740_000_000)));
            rejects(with(&json, "timestamp", json!("yesterday")));
            rejects(with(&json, "exit_code", json!(256)));
            rejects(with(&json, "exit_code", json!(-1)));
            rejects(with(&json, "select_warning", json!(1)));
        }

        #[test]
        fn unknown_keys_are_ignored_so_a_newer_writer_stays_readable() {
            // A later agcli may add a field; a reader on this version must
            // still decode everything it knows.
            let mut success_json = serde_json::to_value(success()).unwrap();
            success_json["surprise"] = json!(1);
            success_json["next_actions"][0]["surprise"] = json!(true);
            let decoded: SuccessEnvelope =
                serde_json::from_value(success_json).expect("extra keys are ignored");
            assert_eq!(decoded, success());

            let mut error_json = serde_json::to_value(error(ErrorCode::new("E"))).unwrap();
            error_json["surprise"] = json!(1);
            error_json["error"]["extra"] = json!(1);
            let decoded: Envelope =
                serde_json::from_value(error_json).expect("extra keys are ignored");
            assert_eq!(decoded, Envelope::Error(error(ErrorCode::new("E"))));

            let param: ActionParam =
                serde_json::from_value(json!({ "required": true, "pattern": "^x" }))
                    .expect("extra keys are ignored");
            assert_eq!(param, ActionParam::new().required(true));
        }

        #[test]
        fn error_envelope_rejects_a_missing_or_malformed_field() {
            let json = serde_json::to_value(error(ErrorCode::new("E"))).unwrap();
            let rejects = |text: String| {
                assert!(
                    serde_json::from_str::<ErrorEnvelope>(&text).is_err(),
                    "must reject {text}"
                );
            };
            for key in [
                "ok",
                "command",
                "timestamp",
                "exit_code",
                "error",
                "fix",
                "next_actions",
            ] {
                rejects(without(&json, key));
            }
            rejects(with(&json, "ok", json!(true)));
            rejects(with(&json, "timestamp", json!("2025-02-19T21:20:00+00:00")));
            rejects(with(&json, "error", json!({ "message": "x", "code": "X" })));
            rejects(with(
                &json,
                "error",
                json!({ "message": "x", "code": "X", "retryable": "no" }),
            ));
            rejects(with(&json, "next_actions", json!([{ "command": "app" }])));
        }
    }
}
