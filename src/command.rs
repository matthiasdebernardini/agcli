//! [`Command`]: one node of the command tree, and its builder.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::codes::ExitCode;
use crate::command_error::CommandError;
use crate::command_output::CommandOutput;
use crate::command_request::CommandRequest;
use crate::next_action::NextAction;
use crate::stream::StreamWriter;

/// A boxed, sendable future: what every stored handler returns.
pub(crate) type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What a normal or streaming handler resolves to.
pub(crate) type HandlerResult = Result<CommandOutput, CommandError>;

type EnvelopeHandler = dyn Fn(CommandRequest) -> BoxFuture<HandlerResult> + Send + Sync;

type StreamHandler = dyn Fn(CommandRequest, StreamWriter) -> BoxFuture<HandlerResult> + Send + Sync;

/// Handler for a raw passthrough command: it receives the verbatim argv tail
/// and returns the process exit code. See [`Command::raw_handler`].
pub(crate) type RawCommandHandler = dyn Fn(Vec<String>) -> BoxFuture<ExitCode> + Send + Sync;

/// The handler a command runs, and so the output contract it answers with.
#[derive(Clone)]
pub(crate) enum Handler {
    /// [`Command::handler`]: the result becomes one JSON envelope.
    Envelope(Arc<EnvelopeHandler>),
    /// [`Command::stream_handler`]: NDJSON events, then one terminal envelope line.
    Stream(Arc<StreamHandler>),
    /// [`Command::raw_handler`]: the command owns its argv, stdout and exit
    /// code, so the framework dispatches it before parsing and emits no envelope.
    Raw(Arc<RawCommandHandler>),
}

/// One node of the command tree: a name, a description, a usage string, a
/// handler, and subcommands.
///
/// The usage string is also the command's schema. Its `<placeholders>` bound
/// the positional arguments and become `next_action` params, its flags are the
/// only ones the command accepts, and its `[--flag]` brackets mark booleans.
/// The handler decides the output contract: one envelope
/// ([`Command::handler`]), an NDJSON stream ([`Command::stream_handler`]), or
/// the command's own stdout ([`Command::raw_handler`]).
#[derive(Clone)]
pub struct Command {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) usage: Option<String>,
    /// The one handler slot: [`Command::handler`], [`Command::stream_handler`]
    /// and [`Command::raw_handler`] each fill it, the last call winning.
    pub(crate) handler: Option<Handler>,
    pub(crate) default_next_actions: Vec<NextAction>,
    pub(crate) subcommands: BTreeMap<String, Command>,
    /// Which framework built-in this command is, if any. Built-ins get
    /// treatment the public builder cannot express; see [`Builtin`].
    pub(crate) builtin: Option<Builtin>,
    /// True when the handler reads `req.dry_run()` and implements a real
    /// preview. The reserved `--dry-run` flag is advertised on every command,
    /// so a handler that ignores it would mutate while the caller believes it
    /// is previewing; the framework refuses to run unmarked handlers under
    /// `--dry-run` (see `DRY_RUN_UNSUPPORTED`).
    pub(crate) handles_dry_run: bool,
    /// Opt out of unknown-flag rejection for this command (e.g. a passthrough
    /// command forwarding arbitrary flags to another program).
    pub(crate) allow_unknown_flags: bool,
    /// Opt out of extra-positional rejection for this command. Usage strings
    /// with a variadic tail (`...`) opt out implicitly.
    pub(crate) allow_extra_args: bool,
}

/// A command the framework registers itself. A command is at most one
/// built-in, so this is one optional state rather than a flag per built-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    /// The `doctor` command registered by [`AgentCli::doctor`]. The
    /// framework-reserved `--select`/`--compact` projection is not applied to
    /// its result, so narrowing can never strip the actionable `fix` strings
    /// out of an unhealthy report.
    Doctor,
    /// The `skill` command registered by [`AgentCli::skill`]. It renders the
    /// whole command tree, which no handler closure can see (the closure is
    /// owned by the very tree it would have to read), so the framework
    /// dispatches it itself, the way it dispatches root and help.
    Skill,
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("usage", &self.usage)
            .field("default_next_actions", &self.default_next_actions)
            .field("subcommand_count", &self.subcommands.len())
            .field("has_handler", &self.handler.is_some())
            .field("stream", &self.is_stream())
            .field("raw", &self.is_raw())
            .finish_non_exhaustive()
    }
}

impl Command {
    /// A command called `name`, described by `description`.
    ///
    /// The name is the token that selects it on the command line. Agents
    /// choose commands by their descriptions, and `audit()` warns about an
    /// empty one. A command with subcommands and no handler is a group, which
    /// answers with its own subtree.
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            usage: None,
            handler: None,
            default_next_actions: Vec::new(),
            subcommands: BTreeMap::new(),
            builtin: None,
            handles_dry_run: false,
            allow_unknown_flags: false,
            allow_extra_args: false,
        }
    }

    /// Declare that this command's handler reads `req.dry_run()` and
    /// implements a real no-mutation preview. Without this marker the
    /// framework refuses to run the handler when `--dry-run` is passed,
    /// returning a structured `DRY_RUN_UNSUPPORTED` error instead of
    /// silently mutating under a flag that promises a preview. The marker
    /// covers this command's subcommands too.
    #[must_use]
    pub fn handles_dry_run(mut self) -> Self {
        self.handles_dry_run = true;
        self
    }

    /// Opt this command out of unknown-flag rejection (for passthrough-style
    /// commands that forward arbitrary flags elsewhere).
    #[must_use]
    pub fn allow_unknown_flags(mut self) -> Self {
        self.allow_unknown_flags = true;
        self
    }

    /// Opt this command out of extra-positional rejection. By default the
    /// framework counts the `<...>` placeholders in the usage string and
    /// refuses surplus positionals with `EXTRA_ARG`, because a silently
    /// dropped argument is how `tool delete <id>` run with two ids deletes the
    /// wrong thing. Usage strings with a variadic tail (`...`) opt out
    /// implicitly, and the opt-out covers this command's subcommands too.
    #[must_use]
    pub fn allow_extra_args(mut self) -> Self {
        self.allow_extra_args = true;
        self
    }

    /// Set the usage template, such as `app get <id> [--fields=<list>]`.
    ///
    /// It documents the command in the tree, the skill and help, and it is
    /// the schema the framework enforces: `<name>` is a required positional,
    /// `[<name>]` an optional one, `--flag <v>` and `--flag=<v>` value flags,
    /// `[--flag]` a boolean that never takes the next token, and a `...` tail
    /// lifts the positional limit. Any flag it does not declare is
    /// `UNKNOWN_FLAG`. Start it with the program name, since agents copy
    /// templates as printed. Without one the command gets no flag or arity
    /// checks, and `audit()` warns.
    #[must_use]
    pub fn usage(mut self, usage: impl Into<String>) -> Self {
        self.usage = Some(usage.into());
        self
    }

    /// Attach an async handler. The closure takes the [`CommandRequest`] by
    /// value and returns any `Send + 'static` future, usually an `async move`
    /// block:
    ///
    /// ```
    /// use agcli::{Command, CommandOutput};
    /// use serde_json::json;
    ///
    /// let logs = Command::new("logs", "Show logs")
    ///     .usage("ops logs <source> [--lines=<n>]")
    ///     .handler(|req| async move {
    ///         let source = req.require_arg(0, "source")?;
    ///         let lines = req.flag_parse::<usize>("lines")?.unwrap_or(20);
    ///         Ok(CommandOutput::new(json!({ "source": source, "lines": lines })))
    ///     });
    /// # let _ = logs;
    /// ```
    ///
    /// The framework boxes the future. The closure is `Fn`, so it may run
    /// more than once: state it owns reaches the future by a clone per call,
    /// `move |req| { let db = Arc::clone(&db); async move { .. } }`, not by a
    /// move out of the closure. A command has one handler slot: a later
    /// [`Command::stream_handler`] or [`Command::raw_handler`] call replaces
    /// this handler.
    #[must_use]
    pub fn handler<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(CommandRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CommandOutput, CommandError>> + Send + 'static,
    {
        self.handler = Some(Handler::Envelope(Arc::new(move |req| Box::pin(f(req)))));
        self
    }

    /// Attach a **streaming handler**: the command answers with NDJSON, one
    /// JSON object per line.
    ///
    /// The handler writes progress through the [`StreamWriter`] it is handed:
    /// `start`, `step`, `progress`, `log` and `event` lines, each stamped with
    /// its `ts`. It returns the same `Result` a normal [`Command::handler`]
    /// does. When it returns, the framework writes exactly
    /// one terminal line: the normal envelope with `"type":"result"` or
    /// `"type":"error"` in front. The terminal line is always last and always
    /// alone, and `--select`, `--quiet` and `next_actions` apply to it as they
    /// do to any envelope.
    ///
    /// Under [`AgentCli::run_env`](crate::AgentCli::run_env) the events reach
    /// stdout as they are written. Under [`AgentCli::run`](crate::AgentCli::run)
    /// they are captured in the [`Execution`](crate::Execution) instead, for a
    /// test to read back with [`Execution::stream_events`](crate::Execution::stream_events).
    ///
    /// ```
    /// use agcli::{Command, CommandOutput, StepStatus, StreamEvent};
    /// use serde_json::json;
    ///
    /// let deploy = Command::new("deploy", "Deploy a service")
    ///     .usage("ops deploy <service>")
    ///     .stream_handler(|req, out| async move {
    ///         let service = req.require_arg(0, "service")?.to_string();
    ///         out.emit(&StreamEvent::Step {
    ///             name: "build".to_string(),
    ///             status: StepStatus::Started,
    ///             duration_ms: None,
    ///             error: None,
    ///         })?;
    ///         Ok(CommandOutput::new(json!({ "service": service, "deployed": true })))
    ///     });
    /// # let _ = deploy;
    /// ```
    ///
    /// A write that fails, because the reader went away, is a
    /// [`StreamWriteError`](crate::StreamWriteError), which `?` turns into a
    /// `STREAM_WRITE_FAILED` [`CommandError`]. The command is marked
    /// `"stream": true` in the root tree and in `help`, and the generated skill
    /// says it streams. A command has one handler slot: a later
    /// [`Command::handler`] or [`Command::raw_handler`] call replaces this
    /// handler.
    #[must_use]
    pub fn stream_handler<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(CommandRequest, StreamWriter) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CommandOutput, CommandError>> + Send + 'static,
    {
        self.handler = Some(Handler::Stream(Arc::new(move |req, out| {
            Box::pin(f(req, out))
        })));
        self
    }

    /// Attach a **raw passthrough handler**: the command owns its own argv,
    /// its own stdout, and its own exit code.
    ///
    /// Some commands are defined by a foreign output contract the JSON
    /// envelope cannot express: a `grep` that must print `path:line:content`
    /// and exit 1 for "no matches", a `cat`, a `sh -c` shim. Wrapping those in
    /// an envelope does not make them agent-native; it makes them wrong. A raw
    /// command opts out of the envelope entirely and keeps the contract it
    /// promised:
    ///
    /// - **Verbatim argv.** The handler receives every token after the command
    ///   path, unparsed and in order: patterns starting with `-`, `-C 3`,
    ///   `-t rust`, `-g '*.rs'`, `--`. Nothing is consumed, reordered or
    ///   rejected. Flag validation, positional-arity checks, the `--dry-run`
    ///   gate and the reserved `--select` / `--compact` / `--quiet` projection
    ///   are all skipped, and unknown flags are the handler's business.
    /// - **Raw stdout.** The framework writes nothing. Print from the handler.
    /// - **Own exit code.** The [`ExitCode`] the future resolves to becomes
    ///   the process status, so `1` can mean "no hits" rather than "failure".
    ///   To forward a child's status, convert `status.code()` with
    ///   `ExitCode::try_from`; it is `None` when a signal killed the child,
    ///   and what that means is the command's to decide.
    ///
    /// The guarantee holds wherever the command's name sits on the line.
    /// `app grep -h pat`, `app --json grep -h pat`, and even a line the
    /// parser rejects outright (`app --json grep --=x`) all reach the handler
    /// with their own tokens. `--help`, `-h`, `--version` and `-V` are the
    /// command's too; ask the framework with `app help grep` instead.
    ///
    /// The tokens before the command's name are the framework's. A help or
    /// version flag there is answered, a reserved boolean given a value that
    /// is not a boolean is `INVALID_FLAG`, a malformed token is
    /// `PARSE_ERROR`, and a flag that is neither reserved nor the framework's
    /// own is `UNKNOWN_FLAG`: `app --bogus=1 grep pat` does not run the
    /// handler. The handler receives only the tokens after its name, so a
    /// reserved agent flag before the name could reach nothing and is refused
    /// instead: `--dry-run` is `DRY_RUN_UNSUPPORTED` (pass the command's own
    /// preview flag after its name, if it has one), and every other one but
    /// `--json` is `INVALID_FLAG`. Nothing runs. Under
    /// [`AgentCli::without_reserved_flags`](crate::AgentCli::without_reserved_flags)
    /// no flag there is refused.
    ///
    /// One shape cannot work: a flag before the command's name that the
    /// framework does not know as a boolean (not a reserved flag, not a
    /// `[--flag]` in a usage string) takes the next token as its value, as
    /// `--key value` always does. `app --profile grep pat` reads `grep` as the
    /// value of `--profile`, so no raw command is named and the line goes to
    /// normal dispatch. Put such flags after the command's name, where they
    /// are the handler's.
    ///
    /// The command still appears in the root command tree (marked
    /// `"raw": true`), in `help`, and in
    /// [`AgentCli::audit`](crate::AgentCli::audit): it is a normal member of
    /// the CLI, and only its output contract differs.
    ///
    /// ```
    /// use agcli::{Command, ExitCode};
    ///
    /// let grep = Command::new("grep", "Search the index (ripgrep-compatible output)")
    ///     .usage("app grep [rg-flags...] <pattern> [path...]")
    ///     .raw_handler(|args| async move {
    ///         let hits: Vec<&String> = args.iter().filter(|arg| arg.contains("fn")).collect();
    ///         for hit in &hits {
    ///             println!("{hit}");
    ///         }
    ///         ExitCode::new(u8::from(hits.is_empty())) // rg's convention: 1 = no matches
    ///     });
    /// # let _ = grep;
    /// ```
    ///
    /// A raw command is a leaf: anything declared under it is unreachable,
    /// and [`AgentCli::audit`](crate::AgentCli::audit) reports it. A command
    /// has one handler slot: a later [`Command::handler`] or
    /// [`Command::stream_handler`] call replaces this handler.
    #[must_use]
    pub fn raw_handler<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Vec<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ExitCode> + Send + 'static,
    {
        self.handler = Some(Handler::Raw(Arc::new(move |args| Box::pin(f(args)))));
        self
    }

    /// True when this command was built with [`Command::raw_handler`] and so
    /// emits raw stdout instead of a JSON envelope.
    #[must_use]
    pub fn is_raw(&self) -> bool {
        self.raw().is_some()
    }

    /// The raw handler, when this command was built with
    /// [`Command::raw_handler`].
    pub(crate) fn raw(&self) -> Option<&RawCommandHandler> {
        self.handler.as_ref().and_then(|handler| match handler {
            Handler::Raw(raw) => Some(&**raw),
            Handler::Envelope(_) | Handler::Stream(_) => None,
        })
    }

    /// True when this command was built with [`Command::stream_handler`] and
    /// so answers with NDJSON events and one terminal envelope line.
    #[must_use]
    pub fn is_stream(&self) -> bool {
        matches!(self.handler, Some(Handler::Stream(_)))
    }

    /// Nest `command` under this one.
    ///
    /// # Panics
    ///
    /// When a subcommand with the same name is already nested here: a second
    /// one would silently replace the first.
    #[must_use]
    pub fn subcommand(mut self, command: Command) -> Self {
        assert!(
            !self.subcommands.contains_key(&command.name),
            "duplicate subcommand: {}",
            command.name
        );
        self.subcommands.insert(command.name.clone(), command);
        self
    }

    /// Add a `next_action` the framework offers when this command's handler,
    /// or a usage error on it, supplies none. Without any, the framework
    /// offers the command's own template and the root. `audit()` checks that
    /// each one names a real command.
    #[must_use]
    pub fn default_next_action(mut self, action: NextAction) -> Self {
        self.default_next_actions.push(action);
        self
    }

    /// The token that selects this command on the command line.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the command does, as the tree, help and the skill show it.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Whether the framework-reserved `--select`/`--compact` projection
    /// applies to this command's result: everywhere except `doctor`.
    pub(crate) fn applies_reserved_projection(&self) -> bool {
        self.builtin != Some(Builtin::Doctor)
    }

    pub(crate) fn usage_or_default(&self, program: &str, path: &[&str]) -> String {
        if let Some(usage) = &self.usage {
            return usage.clone();
        }
        let joined = path.join(" ");
        if self.is_raw() {
            format!("{program} {joined} [args...]")
        } else if !self.subcommands.is_empty() && self.handler.is_none() {
            format!("{program} {joined} <subcommand>")
        } else {
            format!("{program} {joined} [--flag=<value>] [args...]")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "duplicate subcommand")]
    fn duplicate_subcommand_panics() {
        let _ = Command::new("parent", "Parent")
            .subcommand(Command::new("child", "first"))
            .subcommand(Command::new("child", "second"));
    }
}
