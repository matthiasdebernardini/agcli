//! NDJSON streaming: the events a [`Command::stream_handler`] writes, and the
//! [`StreamWriter`] it writes them through.
//!
//! A stream is a sequence of event lines followed by exactly one terminal
//! line, the command's envelope with `"type":"result"` or `"type":"error"` in
//! front. Handlers write only events; the framework writes the terminal line.
//!
//! [`Command::stream_handler`]: crate::Command::stream_handler

use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::mem;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde_json::Value;

use crate::clock::ClockFn;
#[cfg(feature = "deserialize")]
use crate::envelope::{ErrorEnvelope, SuccessEnvelope};
use crate::timestamp::Timestamp;

/// Where a step of a streaming command is, serialized as the `step` event's
/// lowercase `status`.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    /// The step began.
    Started,
    /// The step finished.
    Completed,
    /// The step stopped on an error.
    Failed,
}

/// Log level for stream events, ordered by severity (`Info < Warn < Error`).
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Progress worth knowing.
    Info,
    /// Something the agent may need to act on, though the command goes on.
    Warn,
    /// A failure inside the command.
    Error,
}

/// One event line of an NDJSON stream, as a handler hands it to
/// [`StreamWriter::emit`].
///
/// It serializes with its variant as the `type` key (`"start"`, `"step"`,
/// and so on), its own keys, and then `ts`: the writer stamps every line with
/// the run's clock as it writes it, in RFC 3339 UTC, so an event carries no
/// timestamp of its own. There is no terminal variant. The framework writes
/// the one terminal line from the handler's return value.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum StreamEvent {
    /// The stream began.
    Start {
        /// What is running, usually the command line.
        command: String,
    },
    /// A step of the work started, finished or failed.
    Step {
        /// The step's name, the same on its `started` and `completed` lines.
        name: String,
        /// Where the step is.
        status: StepStatus,
        /// How long the step took, on a `completed` or `failed` line.
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// What went wrong, on a `failed` line.
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// How far the work has got.
    Progress {
        /// What is progressing.
        name: String,
        /// The share done, from 0 to 100.
        #[serde(skip_serializing_if = "Option::is_none")]
        percent: Option<u8>,
        /// A short note on the progress.
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// A diagnostic message.
    Log {
        /// How much the message matters.
        level: LogLevel,
        /// The message.
        message: String,
    },
    /// Something the command observed or emitted, with its data.
    Event {
        /// What happened.
        name: String,
        /// Its payload, any JSON.
        data: Value,
    },
}

/// An event as it goes on the wire: the event's own keys, then `ts`.
#[derive(Serialize)]
struct StampedEvent<'event> {
    #[serde(flatten)]
    event: &'event StreamEvent,
    ts: Timestamp,
}

/// Any one line of an agcli NDJSON stream, for a consumer parsing one.
///
/// Event lines decode to [`StreamLine::Event`]; the terminal line decodes to
/// the envelope it carries. Decoding is strict about what it knows: a
/// missing or malformed known key, or an unknown `type`, is an error. A key it
/// does not know is ignored, so a reader keeps working on the output of a
/// later agcli that only added a field.
#[cfg(feature = "deserialize")]
#[derive(Clone, Debug, PartialEq)]
pub enum StreamLine {
    /// A non-terminal event line.
    Event {
        /// The event.
        event: StreamEvent,
        /// The second its `ts` names.
        timestamp: Timestamp,
    },
    /// The terminal `"type":"result"` line.
    Result(SuccessEnvelope),
    /// The terminal `"type":"error"` line.
    Error(ErrorEnvelope),
}

#[cfg(feature = "deserialize")]
impl<'de> serde::Deserialize<'de> for StreamLine {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let Value::Object(mut line) = Value::deserialize(deserializer)? else {
            return Err(D::Error::custom("a stream line is a JSON object"));
        };
        match line.get("type").and_then(Value::as_str) {
            Some("result") => {
                line.remove("type");
                serde_json::from_value(Value::Object(line))
                    .map(Self::Result)
                    .map_err(D::Error::custom)
            }
            Some("error") => {
                line.remove("type");
                serde_json::from_value(Value::Object(line))
                    .map(Self::Error)
                    .map_err(D::Error::custom)
            }
            Some(_) => {
                let ts = line
                    .remove("ts")
                    .ok_or_else(|| D::Error::missing_field("ts"))?;
                let timestamp = Timestamp::deserialize(ts)
                    .map_err(|e| D::Error::custom(format!("`ts`: {e}")))?;
                let event =
                    serde_json::from_value(Value::Object(line)).map_err(D::Error::custom)?;
                Ok(Self::Event { event, timestamp })
            }
            None => Err(D::Error::custom("missing or invalid `type` field")),
        }
    }
}

/// A failed [`StreamWriter::emit`].
#[derive(Debug)]
#[non_exhaustive]
pub enum StreamWriteError {
    /// Writing or flushing the line failed (`EPIPE` when the reader went
    /// away). The writer is poisoned: a partial line may be on the wire.
    Io(io::Error),
    /// The event could not be serialized. Nothing was written and the writer
    /// stays usable.
    Json(serde_json::Error),
    /// An earlier write failed, so this one was refused rather than appended
    /// to a possibly partial line.
    Poisoned,
    /// The handler already returned and the framework closed the stream, so
    /// the terminal line stays last.
    Closed,
}

impl fmt::Display for StreamWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "stream I/O error: {e}"),
            Self::Json(e) => write!(f, "stream JSON error: {e}"),
            Self::Poisoned => write!(f, "stream poisoned by an earlier failed write"),
            Self::Closed => write!(f, "stream closed: the handler already returned"),
        }
    }
}

impl Error for StreamWriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Poisoned | Self::Closed => None,
        }
    }
}

/// Where a stream's event lines go.
enum Sink {
    /// Written and flushed line by line: stdout under `run_env`.
    Live(Box<dyn Write + Send>),
    /// Kept for the [`Execution`](crate::Execution): in-process `run()`.
    Captured(Vec<String>),
}

struct StreamState {
    sink: Sink,
    poisoned: bool,
    closed: bool,
}

/// The handle a [`Command::stream_handler`](crate::Command::stream_handler)
/// writes its NDJSON events through.
///
/// Synchronous: each [`StreamWriter::emit`] serializes one event, writes it
/// as one line and flushes. Every event is stamped with the run's clock, read
/// as the event is written. Under [`AgentCli::run_env`](crate::AgentCli::run_env)
/// the line goes to stdout; under [`AgentCli::run`](crate::AgentCli::run) it
/// is captured in the [`Execution`](crate::Execution).
///
/// Clones share the stream: a clone handed to a task the handler spawns
/// writes to the same output, one whole line at a time, and is refused with
/// [`StreamWriteError::Closed`] once the handler has returned.
#[derive(Clone)]
pub struct StreamWriter {
    state: Arc<Mutex<StreamState>>,
    /// The run's clock, read for every event's `ts`.
    clock: Arc<ClockFn>,
}

impl fmt::Debug for StreamWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamWriter").finish_non_exhaustive()
    }
}

impl StreamWriter {
    /// A writer whose lines, stamped from `clock`, go straight to `writer`,
    /// plus the framework's handle on it.
    pub(crate) fn live(writer: Box<dyn Write + Send>, clock: Arc<ClockFn>) -> (Self, StreamHandle) {
        Self::with_sink(Sink::Live(writer), clock)
    }

    /// A writer whose lines, stamped from `clock`, are captured, plus the
    /// framework's handle on it.
    pub(crate) fn captured(clock: Arc<ClockFn>) -> (Self, StreamHandle) {
        Self::with_sink(Sink::Captured(Vec::new()), clock)
    }

    fn with_sink(sink: Sink, clock: Arc<ClockFn>) -> (Self, StreamHandle) {
        let state = Arc::new(Mutex::new(StreamState {
            sink,
            poisoned: false,
            closed: false,
        }));
        let handle = StreamHandle {
            state: Arc::clone(&state),
        };
        (Self { state, clock }, handle)
    }

    /// Write `event` as one NDJSON line stamped with the run's clock read now
    /// as its `ts`, and flush it.
    ///
    /// # Errors
    ///
    /// - [`StreamWriteError::Json`] if `event` fails to serialize; nothing is
    ///   written and the writer stays usable.
    /// - [`StreamWriteError::Io`] if the write or flush fails; the writer is
    ///   poisoned, since a partial line may be on the wire.
    /// - [`StreamWriteError::Poisoned`] after an earlier I/O failure.
    /// - [`StreamWriteError::Closed`] once the handler has returned.
    ///
    /// # Panics
    ///
    /// Only if a thread panicked while it held the writer's lock, which the
    /// writer never does: it holds the lock only to check its flags and to
    /// write one line.
    pub fn emit(&self, event: &StreamEvent) -> Result<(), StreamWriteError> {
        let stamped = StampedEvent {
            event,
            ts: (self.clock)(),
        };
        // Serialize before taking the lock: a serde failure leaves nothing on
        // the wire and the writer usable.
        let line = serde_json::to_string(&stamped).map_err(StreamWriteError::Json)?;
        let mut state = lock(&self.state);
        if state.closed {
            return Err(StreamWriteError::Closed);
        }
        if state.poisoned {
            return Err(StreamWriteError::Poisoned);
        }
        match &mut state.sink {
            Sink::Captured(lines) => {
                lines.push(line);
                Ok(())
            }
            Sink::Live(writer) => {
                // One write for the line and its newline, so nothing another
                // thread writes can land between them.
                let mut framed = line;
                framed.push('\n');
                let written = writer
                    .write_all(framed.as_bytes())
                    .and_then(|()| writer.flush());
                if let Err(error) = written {
                    state.poisoned = true;
                    return Err(StreamWriteError::Io(error));
                }
                Ok(())
            }
        }
    }

    /// True once a write failed: every later [`StreamWriter::emit`] is
    /// refused with [`StreamWriteError::Poisoned`].
    ///
    /// # Panics
    ///
    /// Under the same impossible condition as [`StreamWriter::emit`].
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        lock(&self.state).poisoned
    }
}

/// The framework's side of a [`StreamWriter`]: it closes the stream once the
/// handler returns, so nothing can follow the terminal line.
pub(crate) struct StreamHandle {
    state: Arc<Mutex<StreamState>>,
}

impl StreamHandle {
    /// Close the stream and return the lines it captured (none for a live
    /// stream, whose lines already went out).
    pub(crate) fn close(&self) -> Vec<String> {
        let mut state = lock(&self.state);
        state.closed = true;
        match &mut state.sink {
            Sink::Captured(lines) => mem::take(lines),
            Sink::Live(_) => Vec::new(),
        }
    }
}

/// Lock the shared stream state. Poisoning needs a panic while the lock is
/// held, and nothing that runs under it panics, so this propagates one.
fn lock(state: &Mutex<StreamState>) -> MutexGuard<'_, StreamState> {
    state.lock().expect("stream writer lock poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A writer test double that errors on write after `cap` bytes, or on
    /// flush, and shares every byte it accepted so a test can inspect exactly
    /// what reached the wire.
    struct FailingWriter {
        wire: Arc<Mutex<Vec<u8>>>,
        /// Max bytes accepted before every write errors (`None` = no cap).
        write_cap: Option<usize>,
        /// Errors remaining to return from `flush`.
        flush_errors: usize,
    }

    impl Write for FailingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut wire = self.wire.lock().unwrap();
            let room = self.write_cap.map_or(buf.len(), |cap| cap - wire.len());
            if room == 0 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "boom"));
            }
            let take = buf.len().min(room);
            wire.extend_from_slice(&buf[..take]);
            Ok(take)
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.flush_errors > 0 {
                self.flush_errors -= 1;
                return Err(io::Error::other("flush boom"));
            }
            Ok(())
        }
    }

    fn failing(
        write_cap: Option<usize>,
        flush_errors: usize,
    ) -> (StreamWriter, Arc<Mutex<Vec<u8>>>) {
        let wire = Arc::new(Mutex::new(Vec::new()));
        let writer = FailingWriter {
            wire: Arc::clone(&wire),
            write_cap,
            flush_errors,
        };
        let (out, _handle) = StreamWriter::live(Box::new(writer), Arc::new(|| at(1_740_000_000)));
        (out, wire)
    }

    fn at(secs: u64) -> Timestamp {
        Timestamp::from_epoch_secs(secs).expect("in range")
    }

    fn log(message: &str) -> StreamEvent {
        StreamEvent::Log {
            level: LogLevel::Info,
            message: message.to_string(),
        }
    }

    #[test]
    fn emit_writes_one_stamped_line_per_event() {
        let (out, wire) = failing(None, 0);
        out.emit(&StreamEvent::Start {
            command: "app deploy".to_string(),
        })
        .expect("start must emit");
        out.emit(&log("hello")).expect("log must emit");

        let text = String::from_utf8(wire.lock().unwrap().clone()).expect("utf8");
        let lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line is JSON"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["type"], json!("start"));
        assert_eq!(lines[0]["command"], json!("app deploy"));
        assert_eq!(lines[1]["type"], json!("log"));
        // Every event carries the clock's reading (a fixed clock here).
        for line in &lines {
            assert_eq!(line["ts"], json!("2025-02-19T21:20:00Z"), "{line}");
        }
    }

    #[test]
    fn non_terminal_events_serialize_correctly() {
        let step = StreamEvent::Step {
            name: "download".to_string(),
            status: StepStatus::Completed,
            duration_ms: Some(3200),
            error: None,
        };
        let encoded = serde_json::to_value(&step).expect("must serialize");
        assert_eq!(encoded["type"], json!("step"));
        assert_eq!(encoded["status"], json!("completed"));
        assert_eq!(encoded["duration_ms"], json!(3200));
        assert!(encoded.get("error").is_none());
    }

    #[test]
    fn write_error_poisons_writer_and_rejects_retry() {
        // Cap the wire at 10 bytes: the first emit commits a newline-less
        // fragment and fails. The writer must poison itself so a retry cannot
        // concatenate onto the broken line.
        let (out, wire) = failing(Some(10), 0);

        let first = out.emit(&log("this line is longer than ten bytes"));
        assert!(matches!(first, Err(StreamWriteError::Io(_))));
        assert!(out.is_poisoned());

        let retry = out.emit(&log("again"));
        assert!(matches!(retry, Err(StreamWriteError::Poisoned)));

        let wire = wire.lock().unwrap();
        assert_eq!(wire.len(), 10);
        assert!(!wire.ends_with(b"\n"));
    }

    #[test]
    fn flush_error_poisons_writer() {
        let (out, wire) = failing(None, 1);
        let res = out.emit(&log("flushed"));
        assert!(matches!(res, Err(StreamWriteError::Io(_))));
        assert!(out.is_poisoned());
        // The full line did reach the wire before the flush failed.
        assert!(wire.lock().unwrap().ends_with(b"\n"));
    }

    #[test]
    fn closed_stream_refuses_events() {
        let (out, handle) = StreamWriter::captured(Arc::new(|| Timestamp::UNIX_EPOCH));
        out.emit(&log("before"))
            .expect("open stream accepts events");
        let captured = handle.close();
        assert_eq!(captured.len(), 1);

        let late = out.emit(&log("after"));
        assert!(matches!(late, Err(StreamWriteError::Closed)));
        assert!(handle.close().is_empty(), "nothing lands after close");
    }

    #[test]
    fn a_clone_writes_to_the_same_stream_and_closes_with_it() {
        // A handler that spawns work hands the task a clone; the task's lines
        // land in the same stream, and nothing lands after the handler returns.
        let (out, handle) = StreamWriter::captured(Arc::new(|| Timestamp::UNIX_EPOCH));
        let spawned = out.clone();
        let worker = std::thread::spawn(move || spawned.emit(&log("from the task")));
        worker
            .join()
            .expect("the task finishes")
            .expect("an open stream accepts events");
        out.emit(&log("from the handler"))
            .expect("an open stream accepts events");
        assert_eq!(handle.close().len(), 2);

        let kept = out.clone();
        assert!(matches!(
            kept.emit(&log("late")),
            Err(StreamWriteError::Closed)
        ));
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn stream_events_roundtrip_through_stream_line() {
        let events = vec![
            StreamEvent::Start {
                command: "cmd".to_string(),
            },
            StreamEvent::Step {
                name: "step1".to_string(),
                status: StepStatus::Started,
                duration_ms: None,
                error: None,
            },
            StreamEvent::Progress {
                name: "step1".to_string(),
                percent: Some(50),
                message: Some("halfway".to_string()),
            },
            log("watch out"),
            StreamEvent::Event {
                name: "thing".to_string(),
                data: json!({"key": "val"}),
            },
        ];
        let (out, handle) = StreamWriter::captured(Arc::new(|| at(1_740_000_000)));
        for event in &events {
            out.emit(event).expect("must emit");
        }
        let lines = handle.close();
        assert_eq!(lines.len(), events.len());
        for (line, event) in lines.iter().zip(events) {
            let decoded: StreamLine = serde_json::from_str(line).expect("must deserialize");
            assert_eq!(
                decoded,
                StreamLine::Event {
                    event,
                    timestamp: at(1_740_000_000)
                }
            );
        }
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn stream_line_rejects_a_malformed_ts() {
        let line = r#"{"type":"log","level":"info","message":"x","ts":"yesterday"}"#;
        let decoded: Result<StreamLine, _> = serde_json::from_str(line);
        assert!(decoded.is_err());
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn stream_line_rejects_a_missing_key_and_ignores_an_extra_one() {
        let extra =
            r#"{"type":"log","level":"info","message":"x","ts":"2025-02-19T21:20:00Z","extra":1}"#;
        let decoded: StreamLine = serde_json::from_str(extra).expect("an extra key is ignored");
        assert_eq!(
            decoded,
            StreamLine::Event {
                event: log("x"),
                timestamp: at(1_740_000_000)
            }
        );
        for line in [
            // No `ts`.
            r#"{"type":"log","level":"info","message":"x"}"#,
            // No `message`.
            r#"{"type":"log","level":"info","ts":"2025-02-19T21:20:00Z"}"#,
            // No such event type.
            r#"{"type":"heartbeat","ts":"2025-02-19T21:20:00Z"}"#,
            // A terminal line missing its `exit_code`.
            r#"{"type":"result","ok":true,"command":"app","timestamp":"2025-02-19T21:20:00Z","result":null,"next_actions":[]}"#,
            // Not an object.
            r#"["log"]"#,
        ] {
            let decoded: Result<StreamLine, _> = serde_json::from_str(line);
            assert!(decoded.is_err(), "must reject {line}");
        }
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn wire_enums_round_trip() {
        for status in [
            StepStatus::Started,
            StepStatus::Completed,
            StepStatus::Failed,
        ] {
            let json = serde_json::to_string(&status).expect("serialize");
            assert_eq!(
                serde_json::from_str::<StepStatus>(&json).expect("deserialize"),
                status
            );
        }
        for level in [LogLevel::Info, LogLevel::Warn, LogLevel::Error] {
            let json = serde_json::to_string(&level).expect("serialize");
            assert_eq!(
                serde_json::from_str::<LogLevel>(&json).expect("deserialize"),
                level
            );
        }
    }
}
