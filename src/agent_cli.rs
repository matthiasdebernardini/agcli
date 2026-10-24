//! [`AgentCli`]: the CLI definition, its builder, and the envelope and
//! `next_actions` helpers every dispatch path shares.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::clock::{self, ClockFn, InvalidSourceDateEpoch};
use crate::codes::{ErrorCode, ExitCode};
use crate::command::Command;
use crate::command_error::CommandError;
use crate::envelope::{ErrorEnvelope, SuccessEnvelope};
use crate::execution::{Execution, Output};
use crate::invocation::Invocation;
use crate::next_action::NextAction;
use crate::panic_hook::{PanicReport, PanicTarget};
use crate::project;
use crate::reserved_flags;
use crate::select;
use crate::timestamp::Timestamp;
use crate::usage;

/// The definition of an agent-native CLI: its name, description, version and
/// command tree, and the framework behaviour that wraps every command (flag
/// and arity checks, the agent flags, the self-documenting tree, envelopes).
///
/// Build one with [`AgentCli::new`] and [`AgentCli::command`], then hand the
/// process to it with [`AgentCli::run_env`], or drive it in-process with
/// [`AgentCli::run`].
#[derive(Clone)]
pub struct AgentCli {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) version: Option<String>,
    pub(crate) schema_version: Option<String>,
    pub(crate) commands: BTreeMap<String, Command>,
    pub(crate) root_extra: Map<String, Value>,
    pub(crate) reserved_flags: bool,
    /// The clock set with [`AgentCli::clock`]; `None` is the default clock.
    pub(crate) clock: Option<Arc<ClockFn>>,
}

impl fmt::Debug for AgentCli {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentCli")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("version", &self.version)
            .field("command_count", &self.commands.len())
            .field("custom_clock", &self.clock.is_some())
            .finish_non_exhaustive()
    }
}

impl AgentCli {
    /// A CLI called `name`, described by `description`.
    ///
    /// `name` is the program name the usage templates and `next_actions`
    /// start with, and the name of the generated skill. `description` heads
    /// the root tree and the skill. The agent flags start on, and the clock is
    /// the default one.
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            version: None,
            schema_version: None,
            commands: BTreeMap::new(),
            root_extra: Map::new(),
            reserved_flags: true,
            clock: None,
        }
    }

    /// Set the version that `--version`, the root tree and the generated skill
    /// report. Without one, they leave the `version` key out.
    #[must_use]
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Turn off the reserved agent flags: `--select`, `--compact`, `--quiet`,
    /// `--dry-run` and the rest that
    /// [`reserved_flag_names`](crate::reserved_flag_names) lists.
    ///
    /// They are on by default: every command accepts them, and the framework
    /// applies `--select`, `--compact` and `--quiet` to the result. Turn them
    /// off for a CLI whose commands need these names for something else. With
    /// them off the framework also stops checking unknown flags, surplus
    /// positionals and `--dry-run`, projects nothing, and leaves `agent_flags`
    /// out of the root tree and the skill. `--help`, `-h`, `--version` and
    /// `-V` stay reserved.
    #[must_use]
    pub fn without_reserved_flags(mut self) -> Self {
        self.reserved_flags = false;
        self
    }

    /// Set the clock a run reads its timestamps from, in Unix seconds.
    ///
    /// The clock is read whenever a timestamp is written, so each one says
    /// when that thing happened: every event a streaming command writes reads
    /// it as the event is written, the envelope as it is built at the end of
    /// the run, and a `HANDLER_PANIC` envelope at the moment of the panic.
    /// Pin it in a test to make the output byte-stable:
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// use agcli::AgentCli;
    ///
    /// let cli = AgentCli::new("app", "x").clock(|| 0);
    /// let run = cli.run(["app"]).await;
    /// assert!(run.to_json().contains(r#""timestamp":"1970-01-01T00:00:00Z""#));
    /// # }
    /// ```
    ///
    /// Without one, the default clock reads `SOURCE_DATE_EPOCH` when it is
    /// set (the reproducible-builds convention), else the system clock. A
    /// `SOURCE_DATE_EPOCH` that is not a Unix timestamp in seconds fails the
    /// run with an `INVALID_ENV` error envelope (exit code
    /// [`ExitCode::USAGE`]) rather than falling back to the wall clock. A
    /// system clock set before 1970 reads as [`Timestamp::UNIX_EPOCH`].
    ///
    /// # Panics
    ///
    /// The run panics, with a message naming the bound, when the clock reads
    /// past [`Timestamp::MAX`] (253402300799, 9999-12-31T23:59:59Z): no
    /// RFC 3339 timestamp can name that second, and writing a wrong one would
    /// be a silent fallback.
    #[must_use]
    pub fn clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.clock = Some(Arc::new(move || clock::checked(clock())));
        self
    }

    /// Stamp every envelope with `schema_version`, including stream terminal
    /// lines and `HANDLER_PANIC` envelopes, so a consumer can tell which shape
    /// of `result` it is reading. Without one, the key is left out.
    #[must_use]
    pub fn schema_version(mut self, version: impl Into<String>) -> Self {
        self.schema_version = Some(version.into());
        self
    }

    /// Register a top-level command.
    ///
    /// # Panics
    ///
    /// When a command with the same name is already registered: a second
    /// registration would silently replace the first.
    #[must_use]
    pub fn command(mut self, command: Command) -> Self {
        assert!(
            !self.commands.contains_key(&command.name),
            "duplicate command: {}",
            command.name
        );
        self.commands.insert(command.name.clone(), command);
        self
    }

    /// Add `key` to the root tree's `result`, for something an agent should
    /// see first, such as live health. The framework's own keys
    /// (`description`, `version`, `commands`, `agent_flags`, `exit_codes`,
    /// `error_codes`) win over a field of the same name.
    #[must_use]
    pub fn root_field(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.root_extra.insert(key.into(), value.into());
        self
    }

    /// The clock one run reads: the custom one, else the default one, whose
    /// `SOURCE_DATE_EPOCH` pin this validates.
    pub(crate) fn run_clock(&self) -> Result<Arc<ClockFn>, InvalidSourceDateEpoch> {
        self.clock
            .as_ref()
            .map_or_else(clock::default_clock, |clock| Ok(Arc::clone(clock)))
    }

    /// The `INVALID_ENV` answer for a run whose default clock could not be
    /// read. It carries the system clock: the pinned one is what failed.
    pub(crate) fn invalid_env(&self, command: String, error: &InvalidSourceDateEpoch) -> Execution {
        let rerun = NextAction::new(command.clone(), "Re-run once SOURCE_DATE_EPOCH is fixed");
        self.error_execution(
            command,
            &clock::system_now,
            CommandError::new(
                ErrorCode::INVALID_ENV,
                error.to_string(),
                "Unset SOURCE_DATE_EPOCH or set it to a Unix timestamp in seconds",
            )
            .next_action(rerun),
        )
    }

    pub(crate) fn success_envelope(
        &self,
        command: String,
        timestamp: Timestamp,
        result: Value,
        next_actions: Vec<NextAction>,
    ) -> SuccessEnvelope {
        let mut envelope = SuccessEnvelope::new(command, timestamp, result, next_actions);
        if let Some(ref version) = self.schema_version {
            envelope = envelope.schema_version(version);
        }
        envelope
    }

    /// Build a success `Execution` for a framework-rendered (root / help /
    /// version) result, honoring the reserved output flags. The root tree
    /// advertises `--select` / `--compact` / `--quiet` on *every* command, so
    /// these paths must honor them too — the root tree of a large CLI is
    /// exactly where an agent wants `--select=commands`. (The handler path
    /// has its own application with per-command `compact_fields` support.)
    /// The envelope's timestamp is read from `clock` now.
    pub(crate) fn framework_success(
        &self,
        invocation: &Invocation,
        clock: &ClockFn,
        mut result: Value,
        mut next_actions: Vec<NextAction>,
    ) -> Execution {
        let mut select_warning = None;
        if self.reserved_flags {
            if let Some(raw) = invocation.flag("select") {
                (result, select_warning) = select::apply_select_flag(result, raw);
            }
            if reserved_flags::bool_flag_on(invocation.flag("compact")) {
                result = project::compact(&result);
            }
            if reserved_flags::bool_flag_on(invocation.flag("quiet")) {
                next_actions.clear();
            }
        }
        Execution {
            envelope: self
                .success_envelope(
                    invocation.command_line().to_string(),
                    clock(),
                    result,
                    next_actions,
                )
                .select_warning(select_warning)
                .into(),
            output: Output::Envelope,
        }
    }

    pub(crate) fn root_actions(&self) -> Vec<NextAction> {
        if self.commands.is_empty() {
            return vec![NextAction::new(
                self.name.clone(),
                "Inspect root command tree",
            )];
        }

        self.commands
            .values()
            .map(|command| {
                let path = [command.name.as_str()];
                let usage = command.usage_or_default(&self.name, &path);
                usage::next_action_from_usage(&usage, &command.description)
            })
            .collect()
    }

    pub(crate) fn default_command_actions(
        &self,
        path: &[&str],
        command: &Command,
    ) -> Vec<NextAction> {
        if !command.default_next_actions.is_empty() {
            return command.default_next_actions.clone();
        }

        let usage = command.usage_or_default(&self.name, path);
        vec![
            usage::next_action_from_usage(&usage, "Run this command template"),
            NextAction::new(self.name.clone(), "Inspect the full command tree"),
        ]
    }

    pub(crate) fn ensure_next_actions(
        &self,
        actions: Vec<NextAction>,
        path: &[&str],
        command: &Command,
    ) -> Vec<NextAction> {
        if actions.is_empty() {
            self.default_command_actions(path, command)
        } else {
            actions
        }
    }

    pub(crate) fn subcommand_actions(&self, path: &[&str], command: &Command) -> Vec<NextAction> {
        if command.subcommands.is_empty() {
            return self.default_command_actions(path, command);
        }

        let mut path_buf: Vec<&str> = path.to_vec();
        let mut actions = Vec::with_capacity(command.subcommands.len() + 1);
        for sub in command.subcommands.values() {
            path_buf.push(&sub.name);
            let usage = sub.usage_or_default(&self.name, &path_buf);
            actions.push(usage::next_action_from_usage(&usage, &sub.description));
            path_buf.pop();
        }
        actions.push(NextAction::new(
            self.name.clone(),
            "Inspect the full command tree",
        ));
        actions
    }

    /// Build an error `Execution` for a *framework-raised* error. Every such
    /// error (parse failure, unknown command/subcommand, missing handler) is
    /// a usage error, so it carries [`ExitCode::USAGE`] whatever `error` says
    /// and is not retryable: the same invocation fails the same way.
    /// Handler-raised errors do not go through here — they honor the
    /// handler's own exit code and retry hint in `handler_error`. The
    /// envelope's timestamp is read from `clock` now.
    pub(crate) fn error_execution(
        &self,
        command: String,
        clock: &ClockFn,
        error: CommandError,
    ) -> Execution {
        Execution {
            envelope: self
                .build_error_envelope(command, clock(), error.exit_code(ExitCode::USAGE))
                .into(),
            output: Output::Envelope,
        }
    }

    /// The `HANDLER_PANIC` report for one handler run under `run_env`: the
    /// error envelope the panic hook writes, built before the handler runs so
    /// it carries the run's command line, and the clock the hook stamps it
    /// with when the panic happens.
    pub(crate) fn panic_report(
        &self,
        command: String,
        clock: Arc<ClockFn>,
        error: CommandError,
        target: PanicTarget,
    ) -> Arc<PanicReport> {
        // The hook overwrites this reading with the moment of the panic.
        let envelope = self.build_error_envelope(
            command,
            Timestamp::UNIX_EPOCH,
            error.exit_code(ExitCode::ERROR),
        );
        Arc::new(PanicReport {
            envelope,
            target,
            clock,
        })
    }

    /// The [`ErrorEnvelope`] for `error`, with the CLI's `schema_version` if
    /// set.
    pub(crate) fn build_error_envelope(
        &self,
        command: String,
        timestamp: Timestamp,
        error: CommandError,
    ) -> ErrorEnvelope {
        let mut envelope = ErrorEnvelope::new(command, timestamp, error);
        if let Some(ref version) = self.schema_version {
            envelope = envelope.schema_version(version);
        }
        envelope
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Duplicate command assert ---

    #[test]
    #[should_panic(expected = "duplicate command")]
    fn duplicate_command_panics() {
        let _ = AgentCli::new("test", "Test CLI")
            .command(Command::new("status", "first"))
            .command(Command::new("status", "second"));
    }

    #[test]
    fn framework_generated_root_actions_include_params() {
        let cli = AgentCli::new("mycli", "Test CLI").command(
            Command::new("deploy", "Deploy service").usage("mycli deploy <env> [--tag=<tag>]"),
        );
        let actions = cli.root_actions();
        assert_eq!(actions.len(), 1);
        let params = actions[0]
            .params
            .as_ref()
            .expect("params should be present");
        assert_eq!(params.get("env").unwrap().required, Some(true));
        assert_eq!(params.get("tag").unwrap().required, Some(false));
    }
}
