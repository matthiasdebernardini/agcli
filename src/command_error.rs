//! [`CommandError`]: the failure a handler returns.

use std::error::Error;
use std::fmt;

use serde_json::Value;

use crate::codes::{ErrorCode, ExitCode};
use crate::next_action::NextAction;
use crate::stream::StreamWriteError;

/// The failure a command handler returns, and the one value every error
/// envelope is built from.
///
/// Outside the crate it is built only through [`CommandError::new`], which
/// requires a `fix`: every error tells the agent how to recover. A struct
/// literal does not compile:
///
/// ```compile_fail
/// use agcli::{CommandError, ErrorCode};
///
/// let error = CommandError {
///     code: ErrorCode::new("NO_FIX"),
///     message: "no fix given".to_string(),
///     fix: String::new(),
///     retryable: false,
///     next_actions: Vec::new(),
///     exit_code: None,
///     data: None,
/// };
/// ```
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
#[must_use]
pub struct CommandError {
    /// The failure class, serialized as `error.code`.
    pub code: ErrorCode,
    /// What went wrong, serialized as `error.message`.
    pub message: String,
    /// How to recover, serialized as the envelope's `fix`.
    pub fix: String,
    /// Whether the same invocation can succeed later, as after a timeout or a
    /// rate limit. Serialized as `error.retryable`.
    pub retryable: bool,
    /// What the agent can run to recover. When empty, the framework offers
    /// the command's default `next_actions` instead.
    pub next_actions: Vec<NextAction>,
    /// Optional typed process exit code. `None` → [`ExitCode::ERROR`]. Set it
    /// with [`CommandError::exit_code`] using an [`ExitCode`] constant so an
    /// agent can branch on the failure class without parsing the message.
    pub exit_code: Option<ExitCode>,
    /// Optional structured payload for the failure. `None` → the envelope has
    /// no `data` key. Set it with [`CommandError::data`] when the failure
    /// carries facts an agent should read rather than parse out of prose: the
    /// three rejected line numbers, the server's reply, the conflicting id.
    ///
    /// Boxed on purpose: a `CommandError` sits in the `Err` arm of every
    /// handler's `Result`, and an inline `Value` would widen that arm for
    /// every command whether or not it carries a payload.
    pub data: Option<Box<Value>>,
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl Error for CommandError {}

impl CommandError {
    /// A failure of class `code`: `message` says what went wrong, `fix` says
    /// how to recover. The code comes first and is its own type, so the two
    /// strings cannot trade places with it.
    ///
    /// ```
    /// use agcli::{CommandError, ErrorCode, ExitCode};
    ///
    /// let error = CommandError::new(
    ///     ErrorCode::new("NOT_FOUND"),
    ///     "no widget with id 7",
    ///     "List widgets with `app ls` and pick an existing id.",
    /// )
    /// .exit_code(ExitCode::NOT_FOUND);
    /// # let _ = error;
    /// ```
    pub fn new(code: ErrorCode, message: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            fix: fix.into(),
            retryable: false,
            next_actions: Vec::new(),
            exit_code: None,
            data: None,
        }
    }

    /// Say whether the failure is transient: `true` tells the agent the same
    /// call can work later, as after a timeout or a rate limit. Without this
    /// call, an error is not retryable.
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Attach a structured payload, serialized as the envelope's `data` key.
    ///
    /// `message` and `fix` are prose an agent reads; `data` is the part it
    /// should act on without parsing. A validation failure that rejected three
    /// annotations can say so in the message and carry the three, with their
    /// reasons, here. Omitted from the envelope when never set.
    ///
    /// ```
    /// use agcli::{CommandError, ErrorCode};
    /// use serde_json::json;
    ///
    /// let error = CommandError::new(
    ///     ErrorCode::new("PARTIAL_REJECT"),
    ///     "2 of 5 comments rejected",
    ///     "Fix the line numbers",
    /// )
    /// .data(json!({ "rejected": [{ "line": 91, "why": "past end of file" }] }));
    /// # let _ = error;
    /// ```
    pub fn data(mut self, data: impl Into<Value>) -> Self {
        self.data = Some(Box::new(data.into()));
        self
    }

    /// Set the process exit code, which is also the envelope's `exit_code`:
    /// the failure class a shell or an agent branches on. Without this call,
    /// an error exits with [`ExitCode::ERROR`].
    ///
    /// # Panics
    ///
    /// When `code` is [`ExitCode::SUCCESS`]: an error envelope with exit code
    /// 0 is contradictory, since a shell or agent reads it as success. Inside
    /// a handler under [`AgentCli::run_env`](crate::AgentCli::run_env), the
    /// panic still ends in a structured `HANDLER_PANIC` envelope.
    pub fn exit_code(mut self, code: ExitCode) -> Self {
        assert_ne!(
            code,
            ExitCode::SUCCESS,
            "an error must not carry a success (0) exit code"
        );
        self.exit_code = Some(code);
        self
    }

    /// Add a `next_action` that leads to the recovery, such as the command
    /// that lists the valid ids. Without any, the framework offers the
    /// command's defaults.
    pub fn next_action(mut self, action: NextAction) -> Self {
        self.next_actions.push(action);
        self
    }

    /// Add several `next_actions` at once, in order. See
    /// [`CommandError::next_action`].
    pub fn next_actions(mut self, actions: impl IntoIterator<Item = NextAction>) -> Self {
        self.next_actions.extend(actions);
        self
    }
}

/// A stream write a [`Command::stream_handler`](crate::Command::stream_handler)
/// could not complete, as the handler's failure: `STREAM_WRITE_FAILED`, so
/// `out.emit(&event)?` works in a handler.
impl From<StreamWriteError> for CommandError {
    fn from(error: StreamWriteError) -> Self {
        Self::new(
            ErrorCode::STREAM_WRITE_FAILED,
            format!("could not write the event stream: {error}"),
            "The stream's reader went away or the output could not be written. \
             Re-run the command with its stdout read to the end.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agent_cli::AgentCli;
    use crate::command::Command;
    use crate::envelope::Envelope;

    #[test]
    #[should_panic(expected = "an error must not carry a success (0) exit code")]
    fn success_exit_code_panics() {
        let _ =
            CommandError::new(ErrorCode::new("BAD"), "bad", "fix it").exit_code(ExitCode::SUCCESS);
    }

    #[tokio::test]
    async fn command_error_uses_handler_code_and_fix() {
        let cli = AgentCli::new("wokhei", "Agent-first Nostr list CLI").command(
            Command::new("publish", "Publish JSON event").handler(|_req| async move {
                Err(CommandError::new(
                    ErrorCode::new("INVALID_EVENT"),
                    "invalid event json",
                    "Pass valid event JSON and required tags.",
                ))
            }),
        );

        let run = cli.run(["wokhei", "publish"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, ErrorCode::new("INVALID_EVENT"));
        assert_eq!(envelope.fix, "Pass valid event JSON and required tags.");
    }

    // --- Typed exit codes ---

    #[tokio::test]
    async fn handler_error_exit_code_is_honored() {
        let cli = AgentCli::new("app", "x").command(Command::new("find", "find").handler(
            |_req| async move {
                Err(
                    CommandError::new(ErrorCode::new("NOT_FOUND"), "nope", "Check the id")
                        .exit_code(ExitCode::NOT_FOUND),
                )
            },
        ));
        let run = cli.run(["app", "find"]).await;
        assert_eq!(run.exit_code(), ExitCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn handler_error_without_exit_code_defaults_to_one() {
        let cli = AgentCli::new("app", "x").command(Command::new("boom", "boom").handler(
            |_req| async move { Err(CommandError::new(ErrorCode::new("BOOM"), "kaboom", "retry")) },
        ));
        let run = cli.run(["app", "boom"]).await;
        assert_eq!(run.exit_code(), ExitCode::ERROR);
    }
}
