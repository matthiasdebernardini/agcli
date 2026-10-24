//! The process panic hook [`AgentCli::run_env`](crate::AgentCli::run_env)
//! installs, so a panicking handler still ends the process with one JSON
//! error envelope.
//!
//! The hook is installed once per process and chains whatever hook was
//! installed before it. It acts only while a handler is running on the
//! panicking thread: [`guard`] marks the thread around the handler call (which
//! covers a handler's synchronous prologue) and around every poll of the
//! future it returns. The mark is the run's own [`PanicReport`], carried by
//! the guarded future itself, so it is right on whichever worker thread polls
//! the future next.
//!
//! Because the hook runs before unwinding starts, this also works under
//! `panic = "abort"`. A stack overflow or an allocation failure aborts
//! without running any hook, so neither can be reported.

use std::cell::RefCell;
use std::fmt;
use std::future;
use std::panic::{self, PanicHookInfo};
use std::process;
use std::sync::{Arc, Once};

use crate::clock::ClockFn;
use crate::codes::ExitCode;
use crate::command::BoxFuture;
use crate::envelope::{Envelope, ErrorEnvelope};
use crate::execution::{self, ProcessStream};

/// How the `HANDLER_PANIC` envelope is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanicTarget {
    /// A normal command: the envelope on stdout.
    Stdout,
    /// A streaming command: the envelope as the terminal
    /// `{"type":"error",..}` line on stdout.
    Stream,
    /// A raw command owns stdout: the envelope on stderr.
    Stderr,
}

/// What the hook writes if the guarded handler panics: the run's
/// `HANDLER_PANIC` envelope (its command line, next actions and fix) waiting
/// for the panic message and its timestamp, where it goes, and the run's
/// clock to read that timestamp from.
pub(crate) struct PanicReport {
    pub(crate) envelope: ErrorEnvelope,
    pub(crate) target: PanicTarget,
    pub(crate) clock: Arc<ClockFn>,
}

impl fmt::Debug for PanicReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PanicReport")
            .field("envelope", &self.envelope)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

thread_local! {
    /// The report of the handler running on this thread, if one is.
    static ACTIVE: RefCell<Option<Arc<PanicReport>>> = const { RefCell::new(None) };
}

static INSTALL: Once = Once::new();

/// Install the hook, once per process. Later calls do nothing.
pub(crate) fn install() {
    INSTALL.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            previous(info);
            if let Some(report) = active_report() {
                report_panic(&report, info);
            }
        }));
    });
}

/// The report marked on this thread. None when no guarded handler is running
/// here, including while the thread's locals are being torn down.
fn active_report() -> Option<Arc<PanicReport>> {
    match ACTIVE.try_with(|active| active.try_borrow().map(|report| report.clone())) {
        Ok(Ok(report)) => report,
        Ok(Err(_)) | Err(_) => None,
    }
}

/// Write the run's `HANDLER_PANIC` envelope, stamped with the clock's reading
/// now, and exit with [`ExitCode::ERROR`].
fn report_panic(report: &PanicReport, info: &PanicHookInfo<'_>) -> ! {
    let payload = info.payload();
    let detail = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic");
    let mut envelope = report.envelope.clone();
    envelope.error.message = format!("handler panicked: {detail}");
    envelope.timestamp = (report.clock)();
    let envelope = Envelope::Error(envelope);
    match report.target {
        PanicTarget::Stdout => execution::write_line(ProcessStream::Stdout, &envelope.to_json()),
        PanicTarget::Stream => {
            execution::write_line(ProcessStream::Stdout, &envelope.to_stream_json());
        }
        PanicTarget::Stderr => execution::write_line(ProcessStream::Stderr, &envelope.to_json()),
    }
    process::exit(i32::from(ExitCode::ERROR.get()))
}

/// Run a handler under the hook: call `start` to build its future, then poll
/// that future to completion, with `report` marked on the current thread for
/// the call and for each poll. With no report (in-process `run()`) nothing is
/// marked and a panic propagates as usual.
pub(crate) async fn guard<T>(
    report: Option<Arc<PanicReport>>,
    start: impl FnOnce() -> BoxFuture<T>,
) -> T {
    let mut future = marked(report.as_ref(), start);
    future::poll_fn(|cx| marked(report.as_ref(), || future.as_mut().poll(cx))).await
}

/// Call `f` with `report` marked on this thread, restoring the previous mark
/// afterwards (also if `f` unwinds).
fn marked<R>(report: Option<&Arc<PanicReport>>, f: impl FnOnce() -> R) -> R {
    let Some(report) = report else {
        return f();
    };
    let previous = ACTIVE.with(|active| active.replace(Some(Arc::clone(report))));
    let _restore = Restore { previous };
    f()
}

/// Puts the previous mark back when dropped.
struct Restore {
    previous: Option<Arc<PanicReport>>,
}

impl Drop for Restore {
    fn drop(&mut self) {
        let previous = self.previous.take();
        ACTIVE.with(|active| *active.borrow_mut() = previous);
    }
}
