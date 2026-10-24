//! [`Execution`]: the outcome of one run, and how it reaches the process
//! streams.

use std::io::{self, Write};
use std::process;

use crate::codes::ExitCode;
use crate::envelope::Envelope;

/// What one run returned: the envelope, how it reaches stdout (one envelope,
/// an NDJSON stream, or nothing after a raw command), and so the exit code.
///
/// [`Execution::finish`] and [`Execution::print`] are how it leaves the
/// process. A test reads it instead: [`Execution::envelope`],
/// [`Execution::to_json`] and [`Execution::stream_events`].
#[derive(Clone, Debug, PartialEq)]
#[must_use = "an Execution carries the exit code; call finish() or print()"]
pub struct Execution {
    pub(crate) envelope: Envelope,
    pub(crate) output: Output,
}

/// How an [`Execution`] reaches stdout.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Output {
    /// One JSON envelope on stdout.
    Envelope,
    /// A [`Command::stream_handler`](crate::Command::stream_handler) ran:
    /// NDJSON event lines, then the envelope as the one terminal line. Holds
    /// the event lines captured under in-process `run()`; under `run_env`
    /// they already went to stdout live, so it is empty.
    Stream(Vec<String>),
    /// A [`Command::raw_handler`](crate::Command::raw_handler) ran and
    /// already wrote its own stdout; the envelope is bookkeeping (the exit
    /// code) and is not printed.
    Raw,
}

/// A process stream [`write_line`] writes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessStream {
    Stdout,
    Stderr,
}

impl Execution {
    /// The same answer as the terminal line of an NDJSON stream, after
    /// `events`.
    pub(crate) fn into_stream(self, events: Vec<String>) -> Self {
        Self {
            envelope: self.envelope,
            output: Output::Stream(events),
        }
    }

    /// The typed envelope the run answered with. After a raw command it is a
    /// bookkeeping success envelope that carries the exit code and is never
    /// printed.
    #[must_use]
    pub fn envelope(&self) -> &Envelope {
        &self.envelope
    }

    /// The process exit code, equal to the envelope's `exit_code`.
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        self.envelope.exit_code()
    }

    /// The envelope as the JSON line [`Execution::print`] writes last: the
    /// plain envelope, or for a streaming command the terminal line with its
    /// `"type":"result"` / `"type":"error"` key.
    #[must_use]
    pub fn to_json(&self) -> String {
        match self.output {
            Output::Stream(_) => self.envelope.to_stream_json(),
            Output::Envelope | Output::Raw => self.envelope.to_json(),
        }
    }

    /// [`Execution::to_json`], indented.
    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        match self.output {
            Output::Stream(_) => self.envelope.to_stream_json_pretty(),
            Output::Envelope | Output::Raw => self.envelope.to_json_pretty(),
        }
    }

    /// True when a raw command ran and already wrote its own stdout.
    ///
    /// The envelope stays available for inspection and tests, since it carries
    /// the exit code, but printing it on stdout would append JSON to output
    /// the command promised would be raw. [`Execution::print`] and
    /// [`Execution::finish`] honor this; a hand-rolled
    /// `println!("{}", run.to_json())` does not.
    #[must_use]
    pub fn is_raw(&self) -> bool {
        matches!(self.output, Output::Raw)
    }

    /// True when a streaming command answered: stdout is NDJSON, ending in
    /// the terminal line [`Execution::to_json`] returns.
    #[must_use]
    pub fn is_stream(&self) -> bool {
        matches!(self.output, Output::Stream(_))
    }

    /// The event lines a streaming command wrote under in-process
    /// [`AgentCli::run`](crate::AgentCli::run), in order, without the
    /// terminal line. Empty for every other execution, and under
    /// [`AgentCli::run_env`](crate::AgentCli::run_env), where the events went
    /// to stdout as they were written.
    #[must_use]
    pub fn stream_events(&self) -> &[String] {
        match &self.output {
            Output::Stream(events) => events,
            Output::Envelope | Output::Raw => &[],
        }
    }

    /// Write this execution to stdout and return the exit code.
    ///
    /// Normal executions print the JSON envelope. Streaming executions print
    /// any captured event lines, then the terminal line. A raw execution
    /// prints nothing: its handler already wrote stdout.
    ///
    /// Printing never panics: a closed stdout (`EPIPE`, nobody is reading) is
    /// ignored, and any other write failure is reported on stderr. The exit
    /// code is returned either way.
    #[must_use]
    pub fn print(&self) -> ExitCode {
        match &self.output {
            Output::Envelope => write_line(ProcessStream::Stdout, &self.to_json()),
            Output::Stream(events) => {
                let mut text = String::new();
                for event in events {
                    text.push_str(event);
                    text.push('\n');
                }
                text.push_str(&self.to_json());
                write_line(ProcessStream::Stdout, &text);
            }
            Output::Raw => {}
        }
        self.exit_code()
    }

    /// [`Execution::print`], then exit the process with the typed exit code.
    /// The one-line ending for `main`:
    ///
    /// ```no_run
    /// # async fn run(cli: agcli::AgentCli) {
    /// cli.run_env().await.finish()
    /// # }
    /// ```
    ///
    /// `std::process::exit` runs no destructors, so a raw handler that buffers
    /// its own writer must flush before it returns. `println!` flushes each
    /// line already.
    pub fn finish(self) -> ! {
        let code = self.print();
        process::exit(i32::from(code.get()))
    }
}

/// Write `text` and a newline to `stream` on a locked handle, then flush.
///
/// Never panics. `EPIPE` means nobody is reading, so it is ignored. Any other
/// failure on stdout is reported on stderr; a failure on stderr has nowhere
/// left to go.
pub(crate) fn write_line(stream: ProcessStream, text: &str) {
    let written = match stream {
        ProcessStream::Stdout => write_flushed(&mut io::stdout().lock(), text),
        ProcessStream::Stderr => write_flushed(&mut io::stderr().lock(), text),
    };
    if let Err(error) = written
        && error.kind() != io::ErrorKind::BrokenPipe
        && stream == ProcessStream::Stdout
    {
        // Best effort: if stderr is gone too, there is no one left to tell.
        let _ = writeln!(
            io::stderr().lock(),
            "agcli: could not write to stdout: {error}"
        );
    }
}

fn write_flushed(out: &mut impl Write, text: &str) -> io::Result<()> {
    writeln!(out, "{text}")?;
    out.flush()
}
