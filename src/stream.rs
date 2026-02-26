use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;

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
/// `ok` as a fixed boolean and always include `timestamp`. The `ok` field
/// cannot be set incorrectly — it is derived from the variant.
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
        schema_version: Option<String>,
        result: Value,
        next_actions: Vec<NextAction>,
    },
    Error {
        command: String,
        timestamp: u64,
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
                let mut count = 4;
                if duration_ms.is_some() {
                    count += 1;
                }
                if error.is_some() {
                    count += 1;
                }
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
                let mut count = 3;
                if percent.is_some() {
                    count += 1;
                }
                if message.is_some() {
                    count += 1;
                }
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
                schema_version,
                result,
                next_actions,
            } => {
                let mut count = 6;
                if schema_version.is_some() {
                    count += 1;
                }
                let mut map = serializer.serialize_map(Some(count))?;
                map.serialize_entry("type", "result")?;
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("command", command)?;
                map.serialize_entry("timestamp", timestamp)?;
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
                schema_version,
                error,
                fix,
                next_actions,
            } => {
                let mut count = 7;
                if schema_version.is_some() {
                    count += 1;
                }
                let mut map = serializer.serialize_map(Some(count))?;
                map.serialize_entry("type", "error")?;
                map.serialize_entry("ok", &false)?;
                map.serialize_entry("command", command)?;
                map.serialize_entry("timestamp", timestamp)?;
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
                command: value
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                ts: value
                    .get("ts")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            "step" => Ok(Self::Step {
                name: value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                status: serde_json::from_value(value.get("status").cloned().unwrap_or(Value::Null))
                    .map_err(serde::de::Error::custom)?,
                duration_ms: value.get("duration_ms").and_then(Value::as_u64),
                error: value.get("error").and_then(Value::as_str).map(String::from),
                ts: value
                    .get("ts")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            "progress" => Ok(Self::Progress {
                name: value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                percent: value
                    .get("percent")
                    .and_then(Value::as_u64)
                    .and_then(|v| u8::try_from(v).ok()),
                message: value
                    .get("message")
                    .and_then(Value::as_str)
                    .map(String::from),
                ts: value
                    .get("ts")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            "log" => Ok(Self::Log {
                level: serde_json::from_value(value.get("level").cloned().unwrap_or(Value::Null))
                    .map_err(serde::de::Error::custom)?,
                message: value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                ts: value
                    .get("ts")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            "event" => Ok(Self::Event {
                name: value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                data: value.get("data").cloned().unwrap_or(Value::Null),
                ts: value
                    .get("ts")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            "result" => {
                let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
                if !ok {
                    return Err(serde::de::Error::custom(
                        "expected ok=true for result stream event",
                    ));
                }
                Ok(Self::Result {
                    command: value
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    timestamp: value.get("timestamp").and_then(Value::as_u64).unwrap_or(0),
                    schema_version: value
                        .get("schema_version")
                        .and_then(Value::as_str)
                        .map(String::from),
                    result: value.get("result").cloned().unwrap_or(Value::Null),
                    next_actions: serde_json::from_value(
                        value
                            .get("next_actions")
                            .cloned()
                            .unwrap_or(Value::Array(vec![])),
                    )
                    .map_err(serde::de::Error::custom)?,
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
                    command: value
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    timestamp: value.get("timestamp").and_then(Value::as_u64).unwrap_or(0),
                    schema_version: value
                        .get("schema_version")
                        .and_then(Value::as_str)
                        .map(String::from),
                    error: serde_json::from_value(
                        value.get("error").cloned().unwrap_or(Value::Null),
                    )
                    .map_err(serde::de::Error::custom)?,
                    fix: value
                        .get("fix")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    next_actions: serde_json::from_value(
                        value
                            .get("next_actions")
                            .cloned()
                            .unwrap_or(Value::Array(vec![])),
                    )
                    .map_err(serde::de::Error::custom)?,
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
            schema_version: envelope.schema_version,
            result: envelope.result,
            next_actions: envelope.next_actions,
        }
    }

    pub fn error_from_envelope(envelope: ErrorEnvelope) -> Self {
        Self::Error {
            command: envelope.command,
            timestamp: envelope.timestamp,
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

/// Stateful NDJSON event emitter that enforces terminal `result`/`error` semantics.
pub struct NdjsonEmitter<W: Write> {
    writer: W,
    terminated: bool,
}

impl<W: Write> NdjsonEmitter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            terminated: false,
        }
    }

    pub fn emit(&mut self, event: StreamEvent) -> Result<(), StreamEmitError> {
        if self.terminated {
            return Err(StreamEmitError::AlreadyTerminated);
        }

        let terminal = event.is_terminal();
        serde_json::to_writer(&mut self.writer, &event)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;

        if terminal {
            self.terminated = true;
        }
        Ok(())
    }

    pub fn emit_result(&mut self, envelope: SuccessEnvelope) -> Result<(), StreamEmitError> {
        self.emit(StreamEvent::result_from_envelope(envelope))
    }

    pub fn emit_error(&mut self, envelope: ErrorEnvelope) -> Result<(), StreamEmitError> {
        self.emit(StreamEvent::error_from_envelope(envelope))
    }

    pub fn terminated(&self) -> bool {
        self.terminated
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

    #[test]
    fn emitter_writes_ndjson_lines() {
        let buffer = Vec::<u8>::new();
        let mut emitter = NdjsonEmitter::new(buffer);

        emitter
            .emit(StreamEvent::Start {
                command: "wokhei status --follow".to_string(),
                ts: "2026-02-25T20:00:00Z".to_string(),
            })
            .expect("start event must emit");

        emitter
            .emit_result(SuccessEnvelope::new(
                "wokhei status --follow",
                json!({ "healthy": true }),
                vec![],
            ))
            .expect("terminal result must emit");

        assert!(emitter.terminated());
        let out = String::from_utf8(emitter.into_inner()).expect("utf8");
        assert_eq!(out.lines().count(), 2);
        assert!(out.contains("\"type\":\"start\""));
        assert!(out.contains("\"type\":\"result\""));
    }

    #[test]
    fn emitter_rejects_events_after_terminal() {
        let buffer = Vec::<u8>::new();
        let mut emitter = NdjsonEmitter::new(buffer);
        emitter
            .emit_result(SuccessEnvelope::new(
                "wokhei status",
                json!({ "ok": true }),
                vec![],
            ))
            .expect("terminal result must emit");

        let err = emitter
            .emit(StreamEvent::Start {
                command: "wokhei status".to_string(),
                ts: "2026-02-25T20:00:00Z".to_string(),
            })
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
        assert!(encoded["timestamp"].is_u64());
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
        assert!(encoded["timestamp"].is_u64());
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
