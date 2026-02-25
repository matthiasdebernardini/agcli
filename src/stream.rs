use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::envelope::{ErrorBody, ErrorEnvelope, NextAction, SuccessEnvelope};

/// Step lifecycle status for stream events.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Started,
    Completed,
    Failed,
}

/// Log level for stream events.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Typed NDJSON stream event.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum StreamEvent {
    Start {
        command: String,
        ts: String,
    },
    Step {
        name: String,
        status: StepStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        ts: String,
    },
    Progress {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        percent: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
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
        ok: bool,
        command: String,
        result: Value,
        next_actions: Vec<NextAction>,
    },
    Error {
        ok: bool,
        command: String,
        error: ErrorBody,
        fix: String,
        next_actions: Vec<NextAction>,
    },
}

impl StreamEvent {
    pub fn result_from_envelope(envelope: SuccessEnvelope) -> Self {
        Self::Result {
            ok: true,
            command: envelope.command,
            result: envelope.result,
            next_actions: envelope.next_actions,
        }
    }

    pub fn error_from_envelope(envelope: ErrorEnvelope) -> Self {
        Self::Error {
            ok: false,
            command: envelope.command,
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
    use crate::envelope::SuccessEnvelope;
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
}
