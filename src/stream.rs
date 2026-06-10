use std::io;

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::envelope::{ErrorBody, ErrorEnvelope, NextAction, SuccessEnvelope};

/// Step lifecycle status for stream events.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Started,
    Completed,
    Failed,
}

/// Log level for stream events.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Typed NDJSON stream event.
///
/// Terminal variants (`Result`, `Error`) use custom serialization to inject
/// `ok` as a fixed boolean and always include `timestamp` (emitted as an
/// RFC 3339 UTC string). The `ok` field cannot be set incorrectly — it is
/// derived from the variant. The `ts` on non-terminal variants is
/// caller-supplied; use RFC 3339 UTC (`2026-06-10T14:42:17Z`) to match the
/// terminal events.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Start {
        command: String,
        ts: String,
    },
    Step {
        name: String,
        status: StepStatus,
        duration_ms: Option<u64>,
        error: Option<String>,
        ts: String,
    },
    Progress {
        name: String,
        percent: Option<u8>,
        message: Option<String>,
        ts: String,
    },
    Log {
        level: LogLevel,
        message: String,
        ts: String,
    },
    Event {
        name: String,
        data: Value,
        ts: String,
    },
    Result {
        command: String,
        timestamp: u64,
        exit_code: i32,
        schema_version: Option<String>,
        result: Value,
        next_actions: Vec<NextAction>,
    },
    Error {
        command: String,
        timestamp: u64,
        exit_code: i32,
        schema_version: Option<String>,
        error: ErrorBody,
        fix: String,
        next_actions: Vec<NextAction>,
    },
}

impl Serialize for StreamEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::Start { command, ts } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("type", "start")?;
                map.serialize_entry("command", command)?;
                map.serialize_entry("ts", ts)?;
                map.end()
            }
            Self::Step {
                name,
                status,
                duration_ms,
                error,
                ts,
            } => {
                let count = 4 + usize::from(duration_ms.is_some()) + usize::from(error.is_some());
                let mut map = serializer.serialize_map(Some(count))?;
                map.serialize_entry("type", "step")?;
                map.serialize_entry("name", name)?;
                map.serialize_entry("status", status)?;
                if let Some(d) = duration_ms {
                    map.serialize_entry("duration_ms", d)?;
                }
                if let Some(e) = error {
                    map.serialize_entry("error", e)?;
                }
                map.serialize_entry("ts", ts)?;
                map.end()
            }
            Self::Progress {
                name,
                percent,
                message,
                ts,
            } => {
                let count = 3 + usize::from(percent.is_some()) + usize::from(message.is_some());
                let mut map = serializer.serialize_map(Some(count))?;
                map.serialize_entry("type", "progress")?;
                map.serialize_entry("name", name)?;
                if let Some(p) = percent {
                    map.serialize_entry("percent", p)?;
                }
                if let Some(m) = message {
                    map.serialize_entry("message", m)?;
                }
                map.serialize_entry("ts", ts)?;
                map.end()
            }
            Self::Log { level, message, ts } => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "log")?;
                map.serialize_entry("level", level)?;
                map.serialize_entry("message", message)?;
                map.serialize_entry("ts", ts)?;
                map.end()
            }
            Self::Event { name, data, ts } => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "event")?;
                map.serialize_entry("name", name)?;
                map.serialize_entry("data", data)?;
                map.serialize_entry("ts", ts)?;
                map.end()
            }
            Self::Result {
                command,
                timestamp,
                exit_code,
                schema_version,
                result,
                next_actions,
            } => {
                let count = 7 + usize::from(schema_version.is_some());
                let mut map = serializer.serialize_map(Some(count))?;
                map.serialize_entry("type", "result")?;
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("command", command)?;
                map.serialize_entry("timestamp", &crate::envelope::rfc3339_utc(*timestamp))?;
                map.serialize_entry("exit_code", exit_code)?;
                if let Some(sv) = schema_version {
                    map.serialize_entry("schema_version", sv)?;
                }
                map.serialize_entry("result", result)?;
                map.serialize_entry("next_actions", next_actions)?;
                map.end()
            }
            Self::Error {
                command,
                timestamp,
                exit_code,
                schema_version,
                error,
                fix,
                next_actions,
            } => {
                let count = 8 + usize::from(schema_version.is_some());
                let mut map = serializer.serialize_map(Some(count))?;
                map.serialize_entry("type", "error")?;
                map.serialize_entry("ok", &false)?;
                map.serialize_entry("command", command)?;
                map.serialize_entry("timestamp", &crate::envelope::rfc3339_utc(*timestamp))?;
                map.serialize_entry("exit_code", exit_code)?;
                if let Some(sv) = schema_version {
                    map.serialize_entry("schema_version", sv)?;
                }
                map.serialize_entry("error", error)?;
                map.serialize_entry("fix", fix)?;
                map.serialize_entry("next_actions", next_actions)?;
                map.end()
            }
        }
    }
}

#[cfg(feature = "deserialize")]
fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_owned()
}

#[cfg(feature = "deserialize")]
fn opt_str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Read a `timestamp` that may be the RFC 3339 string current emitters write
/// or the legacy epoch integer (pre-0.11 captures).
#[cfg(feature = "deserialize")]
fn timestamp_field(v: &Value, key: &str, default: u64) -> u64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(default),
        Some(Value::String(s)) => crate::envelope::parse_rfc3339_utc(s).unwrap_or(default),
        _ => default,
    }
}

#[cfg(feature = "deserialize")]
fn i32_field(v: &Value, key: &str, default: i32) -> i32 {
    v.get(key)
        .and_then(Value::as_i64)
        .and_then(|n| i32::try_from(n).ok())
        .unwrap_or(default)
}

#[cfg(feature = "deserialize")]
fn value_field(v: &Value, key: &str) -> Value {
    v.get(key).cloned().unwrap_or(Value::Null)
}

#[cfg(feature = "deserialize")]
fn parse_field<T, E>(v: &Value, key: &str, default: Value) -> Result<T, E>
where
    T: serde::de::DeserializeOwned,
    E: serde::de::Error,
{
    serde_json::from_value(v.get(key).cloned().unwrap_or(default)).map_err(E::custom)
}

#[cfg(feature = "deserialize")]
impl<'de> serde::Deserialize<'de> for StreamEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let type_str = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("missing or invalid `type` field"))?;

        match type_str {
            "start" => Ok(Self::Start {
                command: str_field(&value, "command"),
                ts: str_field(&value, "ts"),
            }),
            "step" => Ok(Self::Step {
                name: str_field(&value, "name"),
                status: parse_field(&value, "status", Value::Null)?,
                duration_ms: value.get("duration_ms").and_then(Value::as_u64),
                error: opt_str_field(&value, "error"),
                ts: str_field(&value, "ts"),
            }),
            "progress" => Ok(Self::Progress {
                name: str_field(&value, "name"),
                percent: value
                    .get("percent")
                    .and_then(Value::as_u64)
                    .and_then(|v| u8::try_from(v).ok()),
                message: opt_str_field(&value, "message"),
                ts: str_field(&value, "ts"),
            }),
            "log" => Ok(Self::Log {
                level: parse_field(&value, "level", Value::Null)?,
                message: str_field(&value, "message"),
                ts: str_field(&value, "ts"),
            }),
            "event" => Ok(Self::Event {
                name: str_field(&value, "name"),
                data: value_field(&value, "data"),
                ts: str_field(&value, "ts"),
            }),
            "result" => {
                let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
                if !ok {
                    return Err(serde::de::Error::custom(
                        "expected ok=true for result stream event",
                    ));
                }
                Ok(Self::Result {
                    command: str_field(&value, "command"),
                    timestamp: timestamp_field(&value, "timestamp", 0),
                    exit_code: i32_field(&value, "exit_code", 0),
                    schema_version: opt_str_field(&value, "schema_version"),
                    result: value_field(&value, "result"),
                    next_actions: parse_field(&value, "next_actions", Value::Array(vec![]))?,
                })
            }
            "error" => {
                let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
                if ok {
                    return Err(serde::de::Error::custom(
                        "expected ok=false for error stream event",
                    ));
                }
                Ok(Self::Error {
                    command: str_field(&value, "command"),
                    timestamp: timestamp_field(&value, "timestamp", 0),
                    exit_code: i32_field(&value, "exit_code", 1),
                    schema_version: opt_str_field(&value, "schema_version"),
                    error: parse_field(&value, "error", Value::Null)?,
                    fix: str_field(&value, "fix"),
                    next_actions: parse_field(&value, "next_actions", Value::Array(vec![]))?,
                })
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown stream event type: {other}"
            ))),
        }
    }
}

impl StreamEvent {
    pub fn result_from_envelope(envelope: SuccessEnvelope) -> Self {
        Self::Result {
            command: envelope.command,
            timestamp: envelope.timestamp,
            exit_code: envelope.exit_code,
            schema_version: envelope.schema_version,
            result: envelope.result,
            next_actions: envelope.next_actions,
        }
    }

    pub fn error_from_envelope(envelope: ErrorEnvelope) -> Self {
        Self::Error {
            command: envelope.command,
            timestamp: envelope.timestamp,
            exit_code: envelope.exit_code,
            schema_version: envelope.schema_version,
            error: envelope.error,
            fix: envelope.fix,
            next_actions: envelope.next_actions,
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Result { .. } | Self::Error { .. })
    }
}

/// NDJSON emit failure.
#[derive(Debug)]
pub enum StreamEmitError {
    Io(io::Error),
    Json(serde_json::Error),
    AlreadyTerminated,
}

impl std::fmt::Display for StreamEmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "stream I/O error: {e}"),
            Self::Json(e) => write!(f, "stream JSON error: {e}"),
            Self::AlreadyTerminated => write!(f, "stream already terminated"),
        }
    }
}

impl std::error::Error for StreamEmitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::AlreadyTerminated => None,
        }
    }
}

impl From<io::Error> for StreamEmitError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for StreamEmitError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Controls when the emitter flushes the underlying writer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlushPolicy {
    /// Flush after every emitted event (default). Ensures real-time visibility.
    #[default]
    Every,
    /// Flush only after terminal events (result/error). Reduces I/O overhead
    /// for high-throughput streaming workloads.
    Terminal,
    /// Never flush automatically; caller manages flushing via `into_inner()`.
    Never,
}

/// Stateful NDJSON event emitter that enforces terminal `result`/`error` semantics.
///
/// Async since v0.8: the writer is `tokio::io::AsyncWrite`, and `emit`,
/// `emit_result`, and `emit_error` are `async fn`s.
pub struct NdjsonEmitter<W: AsyncWrite + Unpin> {
    writer: W,
    terminated: bool,
    /// Set when a write/flush error may have left a partial line on the wire.
    /// A poisoned emitter refuses further events so a retry cannot concatenate
    /// onto a corrupt line. Distinct from `terminated` (a clean terminal event).
    poisoned: bool,
    flush_policy: FlushPolicy,
}

impl<W: AsyncWrite + Unpin> NdjsonEmitter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            terminated: false,
            poisoned: false,
            flush_policy: FlushPolicy::default(),
        }
    }

    /// Set the flush policy for this emitter.
    pub fn with_flush_policy(mut self, policy: FlushPolicy) -> Self {
        self.flush_policy = policy;
        self
    }

    pub async fn emit(&mut self, event: StreamEvent) -> Result<(), StreamEmitError> {
        if self.terminated || self.poisoned {
            return Err(StreamEmitError::AlreadyTerminated);
        }

        let terminal = event.is_terminal();
        // Serialize fully into a buffer first so a *serde* failure cannot leave
        // a partial line on the wire (it returns here with nothing written).
        // This does NOT protect against I/O failures: write_all can commit a
        // prefix of the buffer before erroring, so any write error past this
        // point poisons the emitter (below) to stop compounding corruption.
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        if let Err(e) = self.writer.write_all(&line).await {
            // A partial line may now be on the wire. Refuse further events so a
            // retry cannot concatenate onto the broken line.
            self.poisoned = true;
            return Err(StreamEmitError::Io(e));
        }

        // The line and its newline are fully committed. Mark a terminal event
        // terminated NOW — before the flush — so a flush error cannot leave the
        // emitter open to a second terminal event.
        if terminal {
            self.terminated = true;
        }

        let should_flush = match self.flush_policy {
            FlushPolicy::Every => true,
            FlushPolicy::Terminal => terminal,
            FlushPolicy::Never => false,
        };
        if should_flush && let Err(e) = self.writer.flush().await {
            self.poisoned = true;
            return Err(StreamEmitError::Io(e));
        }

        Ok(())
    }

    pub async fn emit_result(&mut self, envelope: SuccessEnvelope) -> Result<(), StreamEmitError> {
        self.emit(StreamEvent::result_from_envelope(envelope)).await
    }

    pub async fn emit_error(&mut self, envelope: ErrorEnvelope) -> Result<(), StreamEmitError> {
        self.emit(StreamEvent::error_from_envelope(envelope)).await
    }

    /// True once a terminal `result`/`error` line has been written — even if a
    /// subsequent flush failed (the bytes are already committed). A caller can
    /// gate further output on this without risking a second terminal event.
    pub fn terminated(&self) -> bool {
        self.terminated
    }

    /// True if a write or flush error may have left a partial line on the wire.
    /// A poisoned emitter rejects further events with [`StreamEmitError::AlreadyTerminated`].
    pub fn poisoned(&self) -> bool {
        self.poisoned
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{ErrorEnvelope, SuccessEnvelope};
    use serde_json::json;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::AsyncWrite;

    /// A writer test double that can short-write/error on `poll_write` and/or
    /// error on `poll_flush`, and records every byte it accepts so a test can
    /// inspect exactly what reached the wire.
    struct FailingWriter {
        wire: Vec<u8>,
        /// Max bytes accepted per `poll_write` before erroring (`None` = no cap).
        write_cap: Option<usize>,
        /// Errors remaining to return from `poll_write` once the cap is hit.
        write_errors: usize,
        /// Errors remaining to return from `poll_flush`.
        flush_errors: usize,
        flushes: usize,
    }

    impl FailingWriter {
        fn write_cap(cap: usize, errors: usize) -> Self {
            Self {
                wire: Vec::new(),
                write_cap: Some(cap),
                write_errors: errors,
                flush_errors: 0,
                flushes: 0,
            }
        }

        fn flush_fail(errors: usize) -> Self {
            Self {
                wire: Vec::new(),
                write_cap: None,
                write_errors: 0,
                flush_errors: errors,
                flushes: 0,
            }
        }
    }

    impl AsyncWrite for FailingWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            let cap = self.write_cap.unwrap_or(buf.len());
            let take = buf.len().min(cap);
            // Commit the accepted prefix to the wire (models a partial write).
            self.wire.extend_from_slice(&buf[..take]);
            if take < buf.len() && self.write_errors > 0 {
                self.write_errors -= 1;
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "boom")));
            }
            Poll::Ready(Ok(take))
        }

        fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.flushes += 1;
            if self.flush_errors > 0 {
                self.flush_errors -= 1;
                return Poll::Ready(Err(io::Error::other("flush boom")));
            }
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn emitter_writes_ndjson_lines() {
        let buffer = Vec::<u8>::new();
        let mut emitter = NdjsonEmitter::new(buffer);

        emitter
            .emit(StreamEvent::Start {
                command: "wokhei status --follow".to_string(),
                ts: "2026-02-25T20:00:00Z".to_string(),
            })
            .await
            .expect("start event must emit");

        emitter
            .emit_result(SuccessEnvelope::new(
                "wokhei status --follow",
                json!({ "healthy": true }),
                vec![],
            ))
            .await
            .expect("terminal result must emit");

        assert!(emitter.terminated());
        let out = String::from_utf8(emitter.into_inner()).expect("utf8");
        assert_eq!(out.lines().count(), 2);
        assert!(out.contains("\"type\":\"start\""));
        assert!(out.contains("\"type\":\"result\""));
    }

    #[tokio::test]
    async fn emitter_rejects_events_after_terminal() {
        let buffer = Vec::<u8>::new();
        let mut emitter = NdjsonEmitter::new(buffer);
        emitter
            .emit_result(SuccessEnvelope::new(
                "wokhei status",
                json!({ "ok": true }),
                vec![],
            ))
            .await
            .expect("terminal result must emit");

        let err = emitter
            .emit(StreamEvent::Start {
                command: "wokhei status".to_string(),
                ts: "2026-02-25T20:00:00Z".to_string(),
            })
            .await
            .expect_err("must reject further events");
        assert!(matches!(err, StreamEmitError::AlreadyTerminated));
    }

    #[test]
    fn result_from_envelope_carries_timestamp_and_schema_version() {
        let envelope = SuccessEnvelope::new("wokhei status", json!({ "healthy": true }), vec![])
            .schema_version("v1.0");

        let event = StreamEvent::result_from_envelope(envelope);
        let encoded = serde_json::to_value(&event).expect("must serialize");
        assert_eq!(encoded["type"], json!("result"));
        assert_eq!(encoded["ok"], json!(true));
        let ts = encoded["timestamp"]
            .as_str()
            .expect("timestamp is a string");
        assert!(ts.ends_with('Z'), "timestamp must be RFC 3339 UTC: {ts}");
        assert_eq!(encoded["schema_version"], json!("v1.0"));
        assert_eq!(encoded["result"]["healthy"], json!(true));
    }

    #[test]
    fn error_from_envelope_carries_timestamp_and_schema_version() {
        let envelope =
            ErrorEnvelope::new("wokhei publish", "bad json", "INVALID", "fix it", vec![])
                .retryable(true)
                .schema_version("v2.0");

        let event = StreamEvent::error_from_envelope(envelope);
        let encoded = serde_json::to_value(&event).expect("must serialize");
        assert_eq!(encoded["type"], json!("error"));
        assert_eq!(encoded["ok"], json!(false));
        let ts = encoded["timestamp"]
            .as_str()
            .expect("timestamp is a string");
        assert!(ts.ends_with('Z'), "timestamp must be RFC 3339 UTC: {ts}");
        assert_eq!(encoded["schema_version"], json!("v2.0"));
        assert_eq!(encoded["error"]["retryable"], json!(true));
    }

    #[test]
    fn result_without_schema_version_omits_field() {
        let envelope = SuccessEnvelope::new("cmd", json!(null), vec![]);
        let event = StreamEvent::result_from_envelope(envelope);
        let encoded = serde_json::to_value(&event).expect("must serialize");
        assert!(encoded.get("schema_version").is_none());
        assert_eq!(encoded["ok"], json!(true));
    }

    #[test]
    fn non_terminal_events_serialize_correctly() {
        let step = StreamEvent::Step {
            name: "download".to_string(),
            status: StepStatus::Completed,
            duration_ms: Some(3200),
            error: None,
            ts: "2026-02-25T20:00:00Z".to_string(),
        };
        let encoded = serde_json::to_value(&step).expect("must serialize");
        assert_eq!(encoded["type"], json!("step"));
        assert_eq!(encoded["status"], json!("completed"));
        assert_eq!(encoded["duration_ms"], json!(3200));
        assert!(encoded.get("error").is_none());
    }

    #[tokio::test]
    async fn flush_policy_terminal_only_flushes_on_terminal() {
        // Use a Vec<u8> — its flush() is a no-op, but we can verify the data
        // is written correctly and the emitter reaches terminal state.
        let buffer = Vec::<u8>::new();
        let mut emitter = NdjsonEmitter::new(buffer).with_flush_policy(FlushPolicy::Terminal);

        emitter
            .emit(StreamEvent::Log {
                level: LogLevel::Info,
                message: "buffered".to_string(),
                ts: "2026-01-01T00:00:00Z".to_string(),
            })
            .await
            .expect("log event must emit");

        emitter
            .emit_result(SuccessEnvelope::new("cmd", json!(null), vec![]))
            .await
            .expect("terminal result must emit");

        assert!(emitter.terminated());
        let out = String::from_utf8(emitter.into_inner()).expect("utf8");
        assert_eq!(out.lines().count(), 2);
    }

    #[tokio::test]
    async fn flush_policy_never_still_writes_data() {
        let buffer = Vec::<u8>::new();
        let mut emitter = NdjsonEmitter::new(buffer).with_flush_policy(FlushPolicy::Never);

        emitter
            .emit_result(SuccessEnvelope::new("cmd", json!(null), vec![]))
            .await
            .expect("terminal result must emit");

        assert!(emitter.terminated());
        let out = String::from_utf8(emitter.into_inner()).expect("utf8");
        assert!(out.contains("\"type\":\"result\""));
    }

    #[test]
    fn flush_policy_default_is_every() {
        assert_eq!(FlushPolicy::default(), FlushPolicy::Every);
    }

    #[tokio::test]
    async fn write_error_poisons_emitter_and_rejects_retry() {
        // Cap the writer at 10 bytes then error: the first emit commits a
        // newline-less fragment and fails. The emitter must poison itself so a
        // retry cannot concatenate onto the broken line.
        let writer = FailingWriter::write_cap(10, 1);
        let mut emitter = NdjsonEmitter::new(writer);

        let first = emitter
            .emit(StreamEvent::Start {
                command: "app run".to_string(),
                ts: "2026-01-01T00:00:00Z".to_string(),
            })
            .await;
        assert!(matches!(first, Err(StreamEmitError::Io(_))));
        assert!(emitter.poisoned());
        assert!(!emitter.terminated());

        // A retry must be refused rather than appended to the partial line.
        let retry = emitter
            .emit(StreamEvent::Start {
                command: "app run".to_string(),
                ts: "2026-01-01T00:00:00Z".to_string(),
            })
            .await;
        assert!(matches!(retry, Err(StreamEmitError::AlreadyTerminated)));

        // Exactly the accepted prefix is on the wire; nothing was concatenated.
        let wire = emitter.into_inner().wire;
        assert_eq!(wire.len(), 10);
        assert!(!wire.ends_with(b"\n"));
    }

    #[tokio::test]
    async fn flush_failure_on_terminal_still_marks_terminated() {
        // The terminal line is fully written, then flush fails. terminated()
        // must report true so a caller cannot emit a second terminal event.
        let writer = FailingWriter::flush_fail(1);
        let mut emitter = NdjsonEmitter::new(writer);

        let res = emitter
            .emit_result(SuccessEnvelope::new("cmd", json!(null), vec![]))
            .await;
        assert!(matches!(res, Err(StreamEmitError::Io(_))));
        assert!(emitter.terminated());

        // A second terminal event is rejected.
        let second = emitter
            .emit_error(ErrorEnvelope::new("cmd", "x", "X", "fix", vec![]))
            .await;
        assert!(matches!(second, Err(StreamEmitError::AlreadyTerminated)));

        // The full terminal line (with newline) did reach the wire.
        let wire = emitter.into_inner().wire;
        assert!(wire.ends_with(b"\n"));
        assert!(String::from_utf8_lossy(&wire).contains("\"type\":\"result\""));
    }

    #[tokio::test]
    async fn flush_policy_never_does_not_flush() {
        // FlushPolicy::Never must not call flush even on a terminal event — a
        // flush-erroring writer would surface the error if it did.
        let writer = FailingWriter::flush_fail(10);
        let mut emitter = NdjsonEmitter::new(writer).with_flush_policy(FlushPolicy::Never);
        emitter
            .emit_result(SuccessEnvelope::new("cmd", json!(null), vec![]))
            .await
            .expect("Never policy must not flush, so no flush error surfaces");
        assert!(emitter.terminated());
        assert_eq!(emitter.into_inner().flushes, 0);
    }

    #[tokio::test]
    async fn flush_policy_terminal_flushes_only_on_terminal() {
        // A non-terminal event must not flush; the terminal event must.
        let writer = FailingWriter {
            wire: Vec::new(),
            write_cap: None,
            write_errors: 0,
            flush_errors: 0,
            flushes: 0,
        };
        let mut emitter = NdjsonEmitter::new(writer).with_flush_policy(FlushPolicy::Terminal);
        emitter
            .emit(StreamEvent::Log {
                level: LogLevel::Info,
                message: "buffered".to_string(),
                ts: "2026-01-01T00:00:00Z".to_string(),
            })
            .await
            .expect("log must emit");
        // No flush yet for the non-terminal log.
        // (Checked indirectly: a fresh emitter's flush count is observed below.)
        emitter
            .emit_result(SuccessEnvelope::new("cmd", json!(null), vec![]))
            .await
            .expect("terminal result must emit");
        assert_eq!(emitter.into_inner().flushes, 1);
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn stream_result_roundtrips() {
        let envelope =
            SuccessEnvelope::new("test cmd", json!({ "data": 42 }), vec![]).schema_version("v1.0");
        let event = StreamEvent::result_from_envelope(envelope);
        let json = serde_json::to_string(&event).expect("must serialize");
        let decoded: StreamEvent = serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(event, decoded);
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn stream_error_roundtrips() {
        let envelope = ErrorEnvelope::new("test cmd", "oops", "ERR", "fix it", vec![])
            .retryable(true)
            .schema_version("v1.0");
        let event = StreamEvent::error_from_envelope(envelope);
        let json = serde_json::to_string(&event).expect("must serialize");
        let decoded: StreamEvent = serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(event, decoded);
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn stream_result_accepts_legacy_epoch_timestamp() {
        // Pre-0.11 captures carry timestamp as epoch seconds.
        let json = r#"{"type":"result","ok":true,"command":"cmd","timestamp":1740000000,"exit_code":0,"result":null,"next_actions":[]}"#;
        let decoded: StreamEvent = serde_json::from_str(json).expect("must deserialize");
        assert!(matches!(
            decoded,
            StreamEvent::Result {
                timestamp: 1_740_000_000,
                ..
            }
        ));
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn stream_non_terminal_roundtrips() {
        let events = vec![
            StreamEvent::Start {
                command: "cmd".to_string(),
                ts: "2026-01-01T00:00:00Z".to_string(),
            },
            StreamEvent::Step {
                name: "step1".to_string(),
                status: StepStatus::Started,
                duration_ms: None,
                error: None,
                ts: "2026-01-01T00:00:01Z".to_string(),
            },
            StreamEvent::Progress {
                name: "step1".to_string(),
                percent: Some(50),
                message: Some("halfway".to_string()),
                ts: "2026-01-01T00:00:02Z".to_string(),
            },
            StreamEvent::Log {
                level: LogLevel::Warn,
                message: "watch out".to_string(),
                ts: "2026-01-01T00:00:03Z".to_string(),
            },
            StreamEvent::Event {
                name: "thing".to_string(),
                data: json!({"key": "val"}),
                ts: "2026-01-01T00:00:04Z".to_string(),
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).expect("must serialize");
            let decoded: StreamEvent = serde_json::from_str(&json).expect("must deserialize");
            assert_eq!(event, decoded);
        }
    }
}
