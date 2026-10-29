//! Raw passthrough commands: finding one in argv and running it.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::agent_cli::AgentCli;
use crate::clock::ClockFn;
use crate::codes::ErrorCode;
use crate::command::RawCommandHandler;
use crate::command_error::CommandError;
use crate::dispatch::RunMode;
use crate::execution::{Execution, Output};
use crate::invocation::Invocation;
use crate::panic_hook::{self, PanicTarget};

impl AgentCli {
    /// Run a [`Command::raw_handler`](crate::Command::raw_handler): hand it the verbatim argv tail, let it
    /// own stdout, and turn the exit code it returns into an `Execution` that
    /// prints nothing.
    ///
    /// Under `run_env` the handler runs under the panic hook. A panicking raw
    /// handler cannot get a JSON error envelope on stdout, which would corrupt
    /// whatever it printed before dying, so the hook writes it to stderr.
    pub(crate) async fn run_raw(
        &self,
        handler: &RawCommandHandler,
        tail: Vec<String>,
        command_line: String,
        clock: &Arc<ClockFn>,
        mode: RunMode,
    ) -> Execution {
        let report = (mode == RunMode::Process).then(|| {
            self.panic_report(
                command_line.clone(),
                Arc::clone(clock),
                CommandError::new(
                    ErrorCode::HANDLER_PANIC,
                    "handler panicked",
                    "This is a bug in the command handler, not the invocation. \
                     Its stdout is incomplete; do not parse it.",
                ),
                PanicTarget::Stderr,
            )
        });
        let code = panic_hook::guard(report, || handler(tail)).await;

        Execution {
            envelope: self
                .success_envelope(command_line, clock(), raw_result(), Vec::new())
                .exit_code(code)
                .into(),
            output: Output::Raw,
        }
    }

    /// Pass 0 of dispatch: does argv name a raw command starting at `start`?
    /// Its handler, and where its own argv begins.
    ///
    /// Walks argv against the command tree literally, with no flag parsing,
    /// because a raw command's tokens are not the framework's to parse. The
    /// first command on that walk carrying a raw handler wins, and everything
    /// after its name is its argv. A token that names no command ends the
    /// walk: this is not a raw invocation.
    pub(crate) fn resolve_raw_command<'a>(
        &'a self,
        argv: &[String],
        start: usize,
    ) -> Option<(&'a RawCommandHandler, usize)> {
        let mut commands = &self.commands;
        for (idx, token) in argv.iter().enumerate().skip(start) {
            let found = commands.get(token)?;
            if let Some(handler) = found.raw() {
                return Some((handler, idx + 1));
            }
            commands = &found.subcommands;
        }
        None
    }

    /// Last-resort scan for a raw command anywhere in argv, used only when the
    /// parser rejected the line outright.
    ///
    /// Normally the split between "the framework's tokens" and "the command's
    /// tokens" comes from the parse. When there is no parse, the choice is
    /// between handing a raw command tokens it may not own and refusing to run
    /// a command whose whole contract is that the framework does not judge its
    /// syntax. The second is worse, so every start offset is tried in order.
    pub(crate) fn find_raw_command<'a>(
        &'a self,
        argv: &[String],
    ) -> Option<(&'a RawCommandHandler, usize)> {
        (1..argv.len()).find_map(|start| self.resolve_raw_command(argv, start))
    }
}

/// The bookkeeping `result` of a raw execution. The envelope never reaches
/// stdout, so this exists to make an inspected or logged raw `Execution`
/// self-explanatory rather than blank.
fn raw_result() -> Value {
    json!({
        "raw": true,
        "stdout": "written directly by the command; this envelope is not printed",
    })
}

/// Index into argv of the first token after a resolved command path
/// `path_len` positionals deep.
///
/// The Pass-0 scan knows this by construction; this recovers it when a raw
/// command was reached through the parse instead (its name was not the leading
/// token). It reads the parser's recorded positional indices rather than
/// searching argv for the command name: with a raw `grep` under `code` and no
/// top-level `grep`, `app --select grep code grep pat` spells `grep` twice,
/// once as a flag value, and only the parser knows which one was the command.
/// `+ 2` converts a `raw_args` index to an argv index (`+ 1` for the program)
/// and steps past the command token itself.
pub(crate) fn args_start(invocation: &Invocation, path_len: usize) -> Option<usize> {
    let last = path_len.checked_sub(1)?;
    Some(invocation.positional_indices().get(last)? + 2)
}
