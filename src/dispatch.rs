//! Dispatch: argv in, [`Execution`] out. Parsing, command resolution, the
//! validation gates, and running a normal handler.

use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::io;
use std::iter;
use std::mem;
use std::sync::Arc;

use crate::agent_cli::AgentCli;
use crate::clock::ClockFn;
use crate::codes::{ErrorCode, ExitCode};
use crate::command::{Builtin, Command, Handler, RawCommandHandler};
use crate::command_error::CommandError;
use crate::command_output::CommandOutput;
use crate::command_request::CommandRequest;
use crate::execution::{Execution, Output};
use crate::invocation::{self, Invocation, ParseInvocationError};
use crate::panic_hook::{self, PanicTarget};
use crate::project;
use crate::raw;
use crate::redaction;
use crate::reserved_flags;
use crate::select;
use crate::stream::StreamWriter;
use crate::suggest;
use crate::usage::{self, Arity};

/// Which entry point a run came through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunMode {
    /// [`AgentCli::run_env`]: the process itself. Handlers run under the
    /// panic hook and stream events go to stdout live.
    Process,
    /// [`AgentCli::run`]: in-process. Panics propagate and stream events are
    /// captured in the [`Execution`].
    InProcess,
}

/// The fix every `HANDLER_PANIC` envelope of a normal or streaming command carries.
const HANDLER_PANIC_FIX: &str = "This is a bug in the command handler, not the invocation. \
                                 Inspect the root command tree and report the panic.";

impl AgentCli {
    /// Run the CLI as the process: read argv from [`std::env::args_os`],
    /// dispatch it, and return the [`Execution`] for
    /// [`Execution::finish`] to print.
    ///
    /// The process entry point. Like [`AgentCli::run`], it validates the
    /// run's clock first (see [`AgentCli::clock`]). An argument that is not valid
    /// UTF-8 is a `PARSE_ERROR` envelope naming its position (`argument 2 is not valid
    /// UTF-8`), never a lossy conversion. It also installs, once per process,
    /// a panic hook that chains the previously installed one: a handler that
    /// panics, in its async body or in the synchronous code before it, still
    /// ends the process with a `HANDLER_PANIC` error envelope and exit code
    /// [`ExitCode::ERROR`], and this function does not return. The envelope
    /// goes to stdout, as the terminal
    /// line for a streaming command, and to stderr for a raw command, whose
    /// stdout is its own. This holds under `panic = "abort"` too. Panics
    /// outside a handler, and panics in tasks a handler spawned that the
    /// runtime catches, behave as they would without agcli; a stack overflow
    /// or an allocation failure aborts before any hook runs.
    ///
    /// Any executor can drive the returned future.
    ///
    /// ```no_run
    /// # async fn run(cli: agcli::AgentCli) {
    /// cli.run_env().await.finish()
    /// # }
    /// ```
    pub async fn run_env(&self) -> Execution {
        panic_hook::install();
        let argv = match utf8_argv(env::args_os()) {
            Ok(argv) => argv,
            Err(index) => return self.non_utf8_argument(index),
        };
        self.dispatch(argv, RunMode::Process).await
    }

    /// The `PARSE_ERROR` answer to an argv whose argument `index` is not
    /// valid UTF-8.
    fn non_utf8_argument(&self, index: usize) -> Execution {
        let clock = match self.run_clock() {
            Ok(clock) => clock,
            Err(error) => return self.invalid_env(self.name.clone(), &error),
        };
        self.error_execution(
            self.name.clone(),
            &*clock,
            CommandError::new(
                ErrorCode::PARSE_ERROR,
                format!("argument {index} is not valid UTF-8"),
                "Pass every argument as valid UTF-8 text.",
            )
            .next_actions(self.root_actions()),
        )
    }

    /// Run the CLI in-process on `args` (program name first) and return the
    /// [`Execution`] without printing it. For tests and embedding.
    ///
    /// Nothing global is touched: no panic hook is installed, so a panicking
    /// handler panics the caller, and a streaming command's events are
    /// captured in the [`Execution`] (see [`Execution::stream_events`])
    /// instead of reaching stdout.
    pub async fn run<I, S>(&self, args: I) -> Execution
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.dispatch(
            args.into_iter().map(Into::into).collect(),
            RunMode::InProcess,
        )
        .await
    }

    async fn dispatch(&self, mut argv: Vec<String>, mode: RunMode) -> Execution {
        if argv.is_empty() {
            argv.push(self.name.clone());
        }
        // Validate the clock before anything runs: a malformed
        // SOURCE_DATE_EPOCH fails here, and every later reading succeeds.
        let clock = match self.run_clock() {
            Ok(clock) => clock,
            Err(error) => {
                return self.invalid_env(self.opaque_command_line(&argv), &error);
            }
        };

        let invocation = match self.parse_argv(&argv) {
            ParsedArgv::Raw {
                handler,
                args_start,
                prefix,
            } => {
                return self
                    .claim_raw(handler, &argv, args_start, prefix, &clock, mode)
                    .await;
            }
            ParsedArgv::Failed(error) => {
                let (masked_message, masked_fix) = error.masked(&argv[1..]);
                return self.error_execution(
                    self.opaque_command_line(&argv),
                    &*clock,
                    CommandError::new(ErrorCode::PARSE_ERROR, masked_message, masked_fix)
                        .next_actions(self.root_actions()),
                );
            }
            ParsedArgv::Invocation(mut invocation) => {
                self.redact_invocation(&mut invocation, false);
                Arc::new(invocation)
            }
        };

        // Every path below reads the boolean flags, so a value that is not a
        // boolean is refused before any of them can misread it.
        if let Some((name, value)) =
            reserved_flags::invalid_bool_flag(invocation.flags(), self.reserved_flags)
        {
            return self.invalid_bool_execution(&invocation, &*clock, name, value);
        }

        if let Some(execution) = self.framework_route(&invocation, &*clock) {
            return execution;
        }

        let [first, ..] = invocation.positionals() else {
            return self.root_execution(&invocation, &*clock);
        };
        let Some(resolved) = self.resolve_command(invocation.positionals()) else {
            return self.unknown_command_execution(&invocation, &*clock, first);
        };
        let command = resolved.command;
        if let Some(execution) = self.group_execution(&invocation, &*clock, &resolved) {
            return execution;
        }

        let path_strs = path_refs(resolved.path);
        let path_cmds = self.path_command_refs(resolved.path);
        let gate = Gate {
            invocation: &invocation,
            clock: &clock,
            command,
            path_strs: &path_strs,
            path_cmds: &path_cmds,
        };
        if let Some(execution) = self
            .reject_unknown_flags(&gate)
            .or_else(|| self.reject_extra_args(&gate, &resolved))
            .or_else(|| self.reject_unsupported_dry_run(&gate))
        {
            // A streaming command answers in NDJSON whatever happens, so a
            // stream consumer never meets a line without a `type`.
            return if command.is_stream() {
                execution.into_stream(Vec::new())
            } else {
                execution
            };
        }

        // The built-in `skill` command renders the live command tree, so the
        // framework runs it here instead of through a handler: a handler
        // closure is owned by the very tree it would have to read. It sits
        // after the checks above so `skill` gets unknown-flag and extra-
        // argument rejection like every other command.
        if command.builtin == Some(Builtin::Skill) {
            return self.skill_execution(&invocation, &*clock);
        }

        let Some(handler) = &command.handler else {
            return self.error_execution(
                invocation.command_line().to_string(),
                &*clock,
                CommandError::new(
                    ErrorCode::MISSING_HANDLER,
                    "command has no handler",
                    "Attach a handler for this command or route to a subcommand.",
                )
                .next_actions(self.root_actions()),
            );
        };

        let request = CommandRequest::new(
            Arc::clone(&invocation),
            resolved.path.len(),
            Arc::clone(&clock),
        );
        self.run_handler(&gate, handler, request, mode).await
    }

    /// The `command` line for a raw command, whose tokens from `args_start`
    /// on are its own. The tokens before them are the framework's: `prefix`
    /// is their parse, redacted like any other line, and without one they are
    /// command names. `prefix` takes the line too, as its envelopes name it.
    fn raw_command_line(
        &self,
        argv: &[String],
        args_start: usize,
        mut prefix: Option<&mut Invocation>,
    ) -> String {
        let head = match prefix.as_deref_mut() {
            Some(prefix) => {
                self.redact_invocation(prefix, true);
                prefix.command_line().to_string()
            }
            None => invocation::render_command_line(&argv[0], &argv[1..args_start]),
        };
        let tail = redaction::raw_tail_words(&argv[args_start..], self.redact_arg.as_deref());
        let command_line = iter::once(head).chain(tail).collect::<Vec<_>>().join(" ");
        if let Some(prefix) = prefix {
            prefix.set_command_line(command_line.clone());
        }
        command_line
    }

    /// The `command` line for argv the framework cannot read token by token:
    /// a line that failed to parse, or a raw command's, whose tokens are its
    /// own. See [`redaction::opaque_words`].
    pub(crate) fn opaque_command_line(&self, argv: &[String]) -> String {
        let words =
            redaction::opaque_words(&argv[1..], self.redact_arg.as_deref(), |path, token| {
                self.names_command(path, token)
            });
        iter::once(invocation::shell_quote(invocation::program_name(&argv[0])).into_owned())
            .chain(words)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Mask what `invocation` carries that must not be echoed, once the
    /// command it names is known: the value of every flag that command does
    /// not declare (a flag no usage declares is not known to be harmless), of
    /// every [`Command::secret`](crate::Command::secret) flag, and of every
    /// secret positional, then whatever the [`AgentCli::redact_arg`] hook
    /// rewrites. Before a command is known, only the flags the framework
    /// reserves are left alone.
    ///
    /// `before_raw` is true for the tokens before a raw command's name: they
    /// can never reach it, so no usage declares any of them.
    fn redact_invocation(&self, invocation: &mut Invocation, before_raw: bool) {
        let resolved = self
            .resolve_command(invocation.positionals())
            .filter(|_| !before_raw);
        let path_cmds = resolved
            .as_ref()
            .map_or_else(Vec::new, |resolved| self.path_command_refs(resolved.path));
        let path_len = resolved.as_ref().map_or(0, |resolved| resolved.path.len());
        let given = invocation.positionals().len().saturating_sub(path_len);
        let mut declared = HashSet::new();
        for command in &path_cmds {
            if let Some(usage) = &command.usage {
                usage::extract_all_flag_names(usage, &mut declared);
            }
        }
        let reserved = reserved_flags::reserved_flag_names();
        let secret_flag = |name: &str| {
            path_cmds
                .iter()
                .any(|command| command.secrets.iter().any(|secret| secret == name))
        };
        let mask_flag = |name: &str| {
            let known = declared.contains(name)
                || reserved_flags::is_framework_flag(name)
                || (self.reserved_flags && reserved.contains(&name));
            secret_flag(name) || !known
        };
        // Positionals the usage declares, by position. When fewer are given
        // than declared, an optional one may have been skipped, so a
        // positional can sit in a later one's slot: from the first optional
        // on, every one is masked if any later one is secret.
        let (secret_positionals, masked_from) = path_cmds.last().map_or_else(
            || (Vec::new(), None),
            |leaf| {
                let declared = leaf
                    .usage
                    .as_deref()
                    .map_or_else(Vec::new, usage::positional_names);
                let secret: Vec<(usize, bool)> = declared
                    .iter()
                    .enumerate()
                    .filter(|(_, positional)| leaf.secrets.contains(&positional.name))
                    .map(|(index, positional)| (index, positional.variadic))
                    .collect();
                let first_optional = declared.iter().position(|positional| positional.optional);
                let masked_from = first_optional.filter(|&optional| {
                    given < declared.len() && secret.iter().any(|&(at, _)| at >= optional)
                });
                (secret, masked_from)
            },
        );
        let mask_positional = |index: usize| {
            index.checked_sub(path_len).is_some_and(|own| {
                masked_from.is_some_and(|from| own >= from)
                    || secret_positionals
                        .iter()
                        .any(|&(at, variadic)| own == at || (variadic && own > at))
            })
        };
        invocation.redact(self.redact_arg.as_deref(), mask_flag, mask_positional);
    }

    /// Answer a raw command that claimed argv from `args_start` on: name the
    /// line, split off the command's own tokens, and run it.
    async fn claim_raw(
        &self,
        handler: &RawCommandHandler,
        argv: &[String],
        args_start: usize,
        mut prefix: Option<Invocation>,
        clock: &Arc<ClockFn>,
        mode: RunMode,
    ) -> Execution {
        let command_line = self.raw_command_line(argv, args_start, prefix.as_mut());
        let tail = argv[args_start.min(argv.len())..].to_vec();
        self.raw_execution(handler, tail, prefix.as_ref(), command_line, clock, mode)
            .await
    }

    /// Run the raw command that claimed argv from `args_start` on, once the
    /// framework has answered for its own tokens: `prefix`, the parsed line up
    /// to the command's name, when the name was not the leading token. A help
    /// or version flag there is answered, a value that is not a boolean is
    /// refused, a flag neither the framework reserves nor the command's usage
    /// declares is unknown, as on the root line, a flag the usage declares is
    /// refused as one that cannot reach the command, and a reserved flag that
    /// cannot reach the handler is refused. The command's own tokens, after its name, are never read.
    async fn raw_execution(
        &self,
        handler: &RawCommandHandler,
        tail: Vec<String>,
        prefix: Option<&Invocation>,
        command_line: String,
        clock: &Arc<ClockFn>,
        mode: RunMode,
    ) -> Execution {
        if let Some(prefix) = prefix {
            if let Some((name, value)) =
                reserved_flags::invalid_bool_flag(prefix.flags(), self.reserved_flags)
            {
                return self.invalid_bool_execution(prefix, &**clock, name, value);
            }
            if let Some(execution) = self.framework_route(prefix, &**clock) {
                return execution;
            }
            let declared = self.raw_declared_flags(prefix);
            if let Some(execution) = self.framework_flag_error(prefix, &declared, &**clock) {
                return execution;
            }
            if let Some(execution) = self.declared_prefix_flag(prefix, &declared, &**clock) {
                return execution;
            }
            if let Some(execution) = self.undeliverable_prefix_flag(prefix, &**clock) {
                return execution;
            }
        }
        self.run_raw(handler, tail, command_line, clock, mode).await
    }

    /// The refusal of a reserved agent flag before a raw command's name. A
    /// raw handler receives only the tokens after its name, so a flag there
    /// can reach neither it nor an envelope: `--dry-run` is
    /// `DRY_RUN_UNSUPPORTED`, and every other one but `--json`, which changes
    /// nothing anywhere, is `INVALID_FLAG`. Nothing is refused when the
    /// reserved flags are off.
    fn undeliverable_prefix_flag(&self, prefix: &Invocation, clock: &ClockFn) -> Option<Execution> {
        if !self.reserved_flags {
            return None;
        }
        let resolved = self.resolve_command(prefix.positionals())?;
        let path = path_refs(resolved.path);
        let name = path.join(" ");
        let error = if reserved_flags::bool_flag_on(prefix.flag("dry-run")) {
            CommandError::new(
                ErrorCode::DRY_RUN_UNSUPPORTED,
                format!("`{name}` does not support --dry-run before its name"),
                format!(
                    "Nothing was run. A raw command receives only the tokens after its name: \
                     drop --dry-run, or if `{name}` has its own preview flag, pass that after \
                     `{name}`."
                ),
            )
        } else {
            let mut reserved: Vec<&str> = prefix
                .flags()
                .keys()
                .map(String::as_str)
                .filter(|&flag| reserved_flags::is_reserved(flag) && flag != "json")
                .collect();
            reserved.sort_unstable();
            not_delivered_error(&name, reserved.first()?)
        };
        Some(self.error_execution(
            prefix.command_line().to_string(),
            clock,
            error.next_actions(self.default_command_actions(&path, resolved.command)),
        ))
    }

    /// The refusal of a flag before a raw command's name that the command's
    /// own usage declares: it is not unknown, it is the command's, but it
    /// cannot reach the command from there. Reserved flags are left to
    /// [`Self::undeliverable_prefix_flag`], and nothing is refused when the
    /// reserved flags are off, as no other flag there is. `declared` is
    /// [`Self::raw_declared_flags`].
    fn declared_prefix_flag(
        &self,
        prefix: &Invocation,
        declared: &HashSet<String>,
        clock: &ClockFn,
    ) -> Option<Execution> {
        if !self.reserved_flags {
            return None;
        }
        let resolved = self.resolve_command(prefix.positionals())?;
        let mut own: Vec<&str> = prefix
            .flags()
            .keys()
            .map(String::as_str)
            .filter(|&flag| declared.contains(flag) && !reserved_flags::is_reserved(flag))
            .collect();
        own.sort_unstable();
        let path = path_refs(resolved.path);
        let error = not_delivered_error(&path.join(" "), own.first()?);
        Some(self.error_execution(
            prefix.command_line().to_string(),
            clock,
            error.next_actions(self.default_command_actions(&path, resolved.command)),
        ))
    }

    /// The flags the usage of the raw command `prefix` names declares, none
    /// when it has no usage string.
    fn raw_declared_flags(&self, prefix: &Invocation) -> HashSet<String> {
        let mut declared = HashSet::new();
        if let Some(usage) = self
            .resolve_command(prefix.positionals())
            .and_then(|resolved| resolved.command.usage.as_deref())
        {
            usage::extract_all_flag_names(usage, &mut declared);
        }
        declared
    }

    /// Run a normal or streaming handler on `request`, under the panic hook
    /// when the run is the process, and turn what it returns into the
    /// envelope.
    async fn run_handler(
        &self,
        gate: &Gate<'_>,
        handler: &Handler,
        request: CommandRequest,
        mode: RunMode,
    ) -> Execution {
        let report = |target| {
            (mode == RunMode::Process).then(|| {
                self.panic_report(
                    gate.invocation.command_line().to_string(),
                    Arc::clone(gate.clock),
                    CommandError::new(
                        ErrorCode::HANDLER_PANIC,
                        "handler panicked",
                        HANDLER_PANIC_FIX,
                    )
                    .next_actions(self.default_command_actions(gate.path_strs, gate.command)),
                    target,
                )
            })
        };

        match handler {
            Handler::Envelope(handler) => {
                let result =
                    panic_hook::guard(report(PanicTarget::Stdout), || handler(request)).await;
                self.handler_execution(gate, result)
            }
            Handler::Stream(handler) => {
                let clock = Arc::clone(gate.clock);
                let (writer, stream) = match mode {
                    RunMode::Process => StreamWriter::live(Box::new(io::stdout()), clock),
                    RunMode::InProcess => StreamWriter::captured(clock),
                };
                let result =
                    panic_hook::guard(report(PanicTarget::Stream), || handler(request, writer))
                        .await;
                // Close before the terminal line exists: a writer the handler
                // smuggled into a spawned task cannot write after it.
                let events = stream.close();
                self.handler_execution(gate, result).into_stream(events)
            }
            // `parse_argv` claims argv for every command path holding a raw
            // command, so dispatch never reaches one.
            Handler::Raw(_) => unreachable!("a raw command is dispatched before argv is parsed"),
        }
    }

    /// The envelope for what a normal or streaming handler returned.
    fn handler_execution(
        &self,
        gate: &Gate<'_>,
        result: Result<CommandOutput, CommandError>,
    ) -> Execution {
        result.map_or_else(
            |error| self.handler_error(gate, error),
            |output| self.handler_success(gate, output),
        )
    }

    /// Whether the parse treats `flag` as a pure boolean that never takes
    /// the next token: the framework's own flags always, the reserved agent
    /// booleans when they are on, and whatever `declared` lists.
    fn parses_as_bool(&self, declared: &HashSet<String>, flag: &str) -> bool {
        declared.contains(flag)
            || reserved_flags::is_framework_flag(flag)
            || (self.reserved_flags && reserved_flags::is_reserved_bool(flag))
    }

    /// Whether `token`, read after `positionals`, names a command: every
    /// positional so far names a command along one path from the root, none
    /// of them raw (a raw command's tokens are its own), and `token` names a
    /// child of the last, or a top-level command when there is none yet.
    /// Every parse of argv passes this, so a command's name is never a bare
    /// flag's value.
    fn names_command(&self, positionals: &[String], token: &str) -> bool {
        let mut commands = &self.commands;
        for name in positionals {
            let Some(found) = commands.get(name) else {
                return false;
            };
            if found.raw().is_some() {
                return false;
            }
            commands = &found.subcommands;
        }
        commands.contains_key(token)
    }

    /// Parse argv, unless a raw command claims it first. A raw command can
    /// claim argv at three points: before parsing, when parsing fails, and
    /// after parsing when its name was not the leading token. In the last two
    /// the tokens before its name are parsed as the framework's prefix.
    fn parse_argv(&self, argv: &[String]) -> ParsedArgv<'_> {
        // Pass 0: a raw command claims its argv before anything reads it. The
        // scan is purely lexical, with no flag parsing and no `--help`
        // interception, because parsing is exactly what the command opted
        // out of.
        if let Some((handler, args_start)) = self.resolve_raw_command(argv, 1) {
            return ParsedArgv::Raw {
                handler,
                args_start,
                prefix: None,
            };
        }

        let preliminary_path = self.preliminary_command_path(argv);
        let bool_flags = Self::path_bool_flag_set(&preliminary_path);
        let is_bool = |flag: &str| self.parses_as_bool(&bool_flags, flag);
        let names_command =
            |positionals: &[String], token: &str| self.names_command(positionals, token);
        let raw_claim = |handler, args_start| {
            // The tokens up to the command's name are the framework's. They
            // parse whenever the whole line did; after a parse failure, a
            // prefix that fails to parse is the framework's own error.
            Invocation::parse_in_tree(
                argv[..args_start].iter().map(String::as_str),
                is_bool,
                names_command,
            )
            .map_or_else(ParsedArgv::Failed, |prefix| ParsedArgv::Raw {
                handler,
                args_start,
                prefix: Some(prefix),
            })
        };
        let invocation = match Invocation::parse_in_tree(
            argv.iter().map(String::as_str),
            is_bool,
            names_command,
        ) {
            Ok(value) => value,
            Err(error) => {
                // A raw command still owns its argv when a *later* token
                // fails to parse (`app --json grep --=x`): the syntax the
                // parser rejected was never the framework's to judge.
                if let Some((handler, args_start)) = self.find_raw_command(argv) {
                    return raw_claim(handler, args_start);
                }
                return ParsedArgv::Failed(error);
            }
        };

        // Pass 1 for a raw command: its name was not the leading token, so the
        // lexical scan missed it (`app --json grep -h pat`). The parsed
        // positionals give the exact argv index its own arguments start at,
        // and only the tokens before that index are the framework's to
        // answer: a `-h` after the name belongs to the command.
        if let Some(resolved) = self.resolve_command(invocation.positionals()) {
            let path_cmds = self.path_command_refs(resolved.path);
            // First raw command on the walk wins, exactly as in Pass 0.
            if let Some((depth, handler)) = path_cmds
                .iter()
                .enumerate()
                .find_map(|(depth, cmd)| cmd.raw().map(|handler| (depth, handler)))
                && let Some(args_start) = raw::args_start(&invocation, depth + 1)
            {
                return raw_claim(handler, args_start);
            }
        }

        ParsedArgv::Invocation(invocation)
    }

    /// The answer to a framework or reserved boolean flag `name` given
    /// `value`, which is not a boolean. Once the positionals resolve to a
    /// command it answers like a gate error: that command's next actions,
    /// and the terminal line of an NDJSON stream for a streaming command.
    /// Otherwise the root next actions.
    fn invalid_bool_execution(
        &self,
        invocation: &Invocation,
        clock: &ClockFn,
        name: &str,
        value: &str,
    ) -> Execution {
        let error = reserved_flags::not_a_boolean(name, invocation.shown_flag_value(name, value));
        let command_line = invocation.command_line().to_string();
        let Some(resolved) = self.resolve_command(invocation.positionals()) else {
            return self.error_execution(
                command_line,
                clock,
                error.next_actions(self.root_actions()),
            );
        };
        let actions = self.default_command_actions(&path_refs(resolved.path), resolved.command);
        let execution = self.error_execution(command_line, clock, error.next_actions(actions));
        if resolved.command.is_stream() {
            execution.into_stream(Vec::new())
        } else {
            execution
        }
    }

    /// The invocations the framework answers itself whatever follows them:
    /// help, the version flags, and the `version` positional.
    fn framework_route(&self, invocation: &Invocation, clock: &ClockFn) -> Option<Execution> {
        if invocation.help_requested()
            || reserved_flags::bool_flag_on(invocation.flag("help"))
            || reserved_flags::bool_flag_on(invocation.flag("h"))
        {
            return Some(self.help_execution(invocation, clock, invocation.positionals()));
        }

        // `<tool> help [command...]`: the alias every agent guesses first.
        // Skipped when the CLI defines its own `help` command.
        if invocation.positionals().first().map(String::as_str) == Some("help")
            && !self.commands.contains_key("help")
        {
            return Some(self.help_execution(invocation, clock, &invocation.positionals()[1..]));
        }

        // `--version` / `-V` anywhere on the line, as `--help` is answered
        // anywhere. Help wins when both are given.
        if reserved_flags::bool_flag_on(invocation.flag("version"))
            || reserved_flags::bool_flag_on(invocation.flag("V"))
        {
            return Some(self.version_flag_execution(invocation, clock));
        }

        // `<tool> version`: the positional spelling of `--version`. Skipped
        // when the CLI defines its own `version` command.
        if invocation.positionals().first().map(String::as_str) == Some("version")
            && !self.commands.contains_key("version")
        {
            return Some(self.version_alias_execution(invocation, clock));
        }
        None
    }

    /// The answer when the first positional, `unknown`, names no command.
    pub(crate) fn unknown_command_execution(
        &self,
        invocation: &Invocation,
        clock: &ClockFn,
        unknown: &str,
    ) -> Execution {
        let valid: Vec<&str> = self.commands.keys().map(String::as_str).collect();
        self.error_execution(
            invocation.command_line().to_string(),
            clock,
            CommandError::new(
                ErrorCode::UNKNOWN_COMMAND,
                format!("unknown command: {}", invocation.shown_positional(0)),
                suggest::unknown_command_fix("command", unknown, &valid),
            )
            .next_actions(self.root_actions()),
        )
    }

    /// The answer when the resolved command is a group with no handler of
    /// its own: its command tree when invoked bare, an unknown-subcommand
    /// error otherwise. `None` when the command runs itself.
    fn group_execution(
        &self,
        invocation: &Invocation,
        clock: &ClockFn,
        resolved: &ResolvedCommand<'_, '_>,
    ) -> Option<Execution> {
        let command = resolved.command;
        if command.handler.is_some() || command.subcommands.is_empty() {
            return None;
        }
        if resolved.remaining.is_empty() {
            return Some(self.command_tree_execution(invocation, clock, resolved.path, command));
        }
        let path_strs = path_refs(resolved.path);
        let valid: Vec<&str> = command.subcommands.keys().map(String::as_str).collect();
        Some(
            self.error_execution(
                invocation.command_line().to_string(),
                clock,
                CommandError::new(
                    ErrorCode::UNKNOWN_SUBCOMMAND,
                    format!(
                        "unknown subcommand: {}",
                        invocation.shown_positional(resolved.path.len())
                    ),
                    suggest::unknown_command_fix("subcommand", &resolved.remaining[0], &valid),
                )
                .next_actions(self.subcommand_actions(&path_strs, command)),
            ),
        )
    }

    /// The flags the command at the end of `path_cmds` declares, the union
    /// across the path, when unknown flags are rejected for it at all. `None`
    /// when a command on the path opted out via `allow_unknown_flags()`, and
    /// for a command that runs without a usage string: it has no flag schema
    /// to check against. A group never runs, so without a usage string it
    /// declares nothing, and its tree answer refuses a flag nobody declared
    /// instead of dropping it.
    pub(crate) fn declared_flags(path_cmds: &[&Command]) -> Option<HashSet<String>> {
        let leaf = path_cmds.last()?;
        let runs_unchecked = leaf.usage.is_none() && leaf.handler.is_some();
        if runs_unchecked || path_cmds.iter().any(|c| c.allow_unknown_flags) {
            return None;
        }
        let mut declared = HashSet::new();
        for cmd in path_cmds {
            if let Some(usage) = &cmd.usage {
                usage::extract_all_flag_names(usage, &mut declared);
            }
        }
        Some(declared)
    }

    /// The `UNKNOWN_FLAG` error for the flags on `invocation` that neither
    /// `declared` nor `known` lists and the framework does not reserve, or
    /// `None` when there are none or the reserved flags are off. `scope`
    /// names who owns `declared` in the fix (`calc add`, or the program for
    /// the root tree); `known` flags are left out of it, as they are not
    /// `scope`'s.
    ///
    /// Without this, a typo'd flag (`--lmit 3`) is silently dropped and the
    /// command runs with defaults: exit 0, wrong behavior, nothing for the
    /// agent to learn from.
    pub(crate) fn unknown_flag_error(
        &self,
        invocation: &Invocation,
        declared: &HashSet<String>,
        known: &HashSet<String>,
        scope: &str,
    ) -> Option<CommandError> {
        use std::fmt::Write as _;

        if !self.reserved_flags {
            return None;
        }
        let reserved = reserved_flags::reserved_flag_names();
        let allowed = |flag: &str| {
            declared.contains(flag)
                || known.contains(flag)
                || reserved_flags::is_framework_flag(flag)
                || reserved.contains(&flag)
        };
        let mut unknown: Vec<&String> = invocation.flags().keys().filter(|f| !allowed(f)).collect();
        unknown.sort();
        unknown.first()?;
        // A flag set by a token masked whole is not named: its letters would
        // spell the secret.
        let (hidden, unknown): (Vec<&String>, Vec<&String>) = unknown
            .into_iter()
            .partition(|name| invocation.flag_is_hidden(name));
        let mut declared_sorted: Vec<&str> = declared.iter().map(String::as_str).collect();
        declared_sorted.sort_unstable();
        // Typo candidates cover the reserved and framework flags too:
        // `--selct` should nudge toward `--select` even though no command
        // declares it. Declared first, so equal-distance ties resolve
        // deterministically toward the command's own flags.
        let mut candidates = declared_sorted.clone();
        candidates.extend(reserved);
        candidates.extend(["help", "version"]);
        let mut fix = String::new();
        if let Some(near) = unknown
            .first()
            .and_then(|first| suggest::nearest_name(first, &candidates))
        {
            let _ = write!(&mut fix, "Did you mean `{}`? ", usage::flag_display(&near));
        }
        if declared_sorted.is_empty() {
            let _ = write!(&mut fix, "`{scope}` takes no flags of its own.");
        } else {
            let _ = write!(
                &mut fix,
                "Valid flags for `{scope}`: {}.",
                declared_sorted
                    .iter()
                    .map(|n| usage::flag_display(n))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let _ = write!(
            &mut fix,
            " Reserved agent flags ({}) are accepted on every command, as are --help and \
             --version.",
            reserved
                .iter()
                .map(|n| usage::flag_display(n))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let unknown_list = unknown
            .iter()
            .map(|f| usage::flag_display(f))
            .chain((!hidden.is_empty()).then(|| redaction::MARKER.to_string()))
            .collect::<Vec<_>>()
            .join(", ");
        Some(CommandError::new(
            ErrorCode::UNKNOWN_FLAG,
            format!("unknown flag(s): {unknown_list}"),
            fix,
        ))
    }

    /// Reject flags the resolved command never declared (see
    /// [`Self::unknown_flag_error`]).
    fn reject_unknown_flags(&self, gate: &Gate<'_>) -> Option<Execution> {
        let declared = Self::declared_flags(gate.path_cmds)?;
        let error = self.unknown_flag_error(
            gate.invocation,
            &declared,
            &HashSet::new(),
            &gate.path_strs.join(" "),
        )?;
        Some(self.gate_error(gate, error))
    }

    /// Reject surplus positional arguments. Without this, `tool delete
    /// <id>` invoked with two ids silently drops the second: exit 0,
    /// wrong behavior, nothing for the agent to learn from. Bounded by the
    /// `<...>` placeholders in the leaf usage string; variadic usages
    /// (`...`) and `allow_extra_args()` commands opt out.
    fn reject_extra_args(
        &self,
        gate: &Gate<'_>,
        resolved: &ResolvedCommand<'_, '_>,
    ) -> Option<Execution> {
        if self.reserved_flags
            && !gate.path_cmds.iter().any(|c| c.allow_extra_args)
            && let Some(usage) = &gate.command.usage
            && let Some(arity) = usage::positional_arity(usage)
            && resolved.remaining.len() > arity.required + arity.optional
        {
            return Some(
                self.gate_error(
                    gate,
                    extra_arg_error(
                        &gate.path_strs.join(" "),
                        usage,
                        arity,
                        &(0..resolved.remaining.len())
                            .map(|at| {
                                let first =
                                    gate.invocation.positionals().len() - resolved.remaining.len();
                                gate.invocation.shown_positional(first + at).to_string()
                            })
                            .collect::<Vec<_>>(),
                    ),
                ),
            );
        }
        None
    }

    /// `--dry-run` promises "preview without mutating" on every command.
    /// Running a handler that never reads `req.dry_run()` would mutate
    /// under that promise, so refuse unless the command declared support.
    fn reject_unsupported_dry_run(&self, gate: &Gate<'_>) -> Option<Execution> {
        if self.reserved_flags
            && reserved_flags::bool_flag_on(gate.invocation.flag("dry-run"))
            && !gate.path_cmds.iter().any(|c| c.handles_dry_run)
        {
            return Some(self.gate_error(
                gate,
                CommandError::new(
                    ErrorCode::DRY_RUN_UNSUPPORTED,
                    format!("`{}` does not support --dry-run", gate.path_strs.join(" ")),
                    "Nothing was changed. This command has no preview mode: run it without \
                     --dry-run to execute it, or inspect current state first with a read \
                     command from next_actions.",
                ),
            ));
        }
        None
    }

    /// The usage error a validation gate raises for the resolved command,
    /// pointing at that command's templates.
    fn gate_error(&self, gate: &Gate<'_>, error: CommandError) -> Execution {
        self.error_execution(
            gate.invocation.command_line().to_string(),
            &**gate.clock,
            error.next_actions(self.default_command_actions(gate.path_strs, gate.command)),
        )
    }

    /// Turn a handler's successful output into the success envelope: fill in
    /// default next actions, advertise `--select` for list results, and apply
    /// the reserved output flags. The timestamp is read now, as the handler
    /// has returned.
    fn handler_success(&self, gate: &Gate<'_>, output: CommandOutput) -> Execution {
        let Gate {
            invocation,
            clock,
            command,
            path_strs,
            ..
        } = *gate;
        let CommandOutput {
            mut result,
            next_actions: handler_actions,
            exit_code: output_exit,
            compact_fields,
            select_fields,
        } = output;
        let mut next_actions = self.ensure_next_actions(handler_actions, path_strs, command);

        // A list result knows its row schema, so teach the cheaper call:
        // the same invocation re-run under --select. Only when the
        // projection would actually apply and the caller has not already
        // selected: re-advertising a flag in use is context noise.
        let advertise_select = self.reserved_flags
            && command.applies_reserved_projection()
            && invocation.flag("select").is_none();
        if let Some(fields) = select_fields.filter(|fields| advertise_select && !fields.is_empty())
        {
            next_actions.push(select::advertisement_action(invocation, &fields));
        }

        // Apply the framework-reserved output flags centrally so every
        // command supports --select / --compact / --quiet for free.
        let mut select_warning = None;
        if self.reserved_flags && command.applies_reserved_projection() {
            let selected = invocation.value_flag("select");
            if let Some(raw) = selected {
                (result, select_warning) = select::apply_select_flag(result, raw);
            }
            if reserved_flags::bool_flag_on(invocation.flag("compact")) {
                // An explicit --select names what the agent wants, so beside
                // it --compact only drops nulls and empty containers: keeping
                // the command's `compact_fields` instead could empty the
                // selection without a word.
                match &compact_fields {
                    Some(fields) if selected.is_none() => {
                        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
                        result = project::select(&result, &refs);
                    }
                    _ => {
                        let compacted = project::compact(&result);
                        // A selection --compact empties is kept as selected,
                        // with a word, the way a --select miss is. Emptied
                        // counts at any depth: a list's rows survive as `{}`.
                        if selected.is_some()
                            && project::is_hollow(&compacted)
                            && compacted != result
                        {
                            let warning = "--compact removed every selected field because they \
                                           were null or empty. Returning the selection \
                                           uncompacted.";
                            select_warning = Some(select_warning.map_or_else(
                                || warning.to_string(),
                                |previous| format!("{previous} {warning}"),
                            ));
                        } else {
                            result = compacted;
                        }
                    }
                }
            }
        }
        if self.reserved_flags && reserved_flags::bool_flag_on(invocation.flag("quiet")) {
            next_actions.clear();
        }

        let envelope = self
            .success_envelope(
                invocation.command_line().to_string(),
                clock(),
                result,
                next_actions,
            )
            .select_warning(select_warning)
            .exit_code(output_exit.unwrap_or(ExitCode::SUCCESS));
        Execution {
            envelope: envelope.into(),
            output: Output::Envelope,
        }
    }

    /// Turn a handler's `CommandError` into the error envelope, honoring the
    /// handler's own exit code, retry hint and structured payload. The
    /// timestamp is read now, as the handler has returned.
    fn handler_error(&self, gate: &Gate<'_>, mut error: CommandError) -> Execution {
        error.next_actions = self.ensure_next_actions(
            mem::take(&mut error.next_actions),
            gate.path_strs,
            gate.command,
        );
        let envelope = self.build_error_envelope(
            gate.invocation.command_line().to_string(),
            (gate.clock)(),
            error,
        );
        Execution {
            envelope: envelope.into(),
            output: Output::Envelope,
        }
    }

    /// Collect every flag name declared as a pure boolean (`[--flag]`) across
    /// the entire command tree, including subcommands. Used only as a Pass 1
    /// heuristic by [`Self::preliminary_command_path`]. A command's name is
    /// never a flag's value in any parse, so the union only decides whether a
    /// token that names no command, right after a bare flag some command
    /// declares boolean, is a positional (which ends the walk) or that flag's
    /// value. The final flag/positional split honors the resolved command
    /// path's schema, not this global union; see [`Self::path_bool_flag_set`].
    fn global_bool_flag_set(&self) -> HashSet<String> {
        let mut set = HashSet::new();
        for command in self.commands.values() {
            collect_bool_flags(command, &mut set);
        }
        set
    }

    /// Pass 1 of command resolution: walk argv with the global bool-flag
    /// union as a heuristic, then walk the resulting positionals against the
    /// command tree to find the deepest matching command path.
    ///
    /// The result feeds [`Self::path_bool_flag_set`] so the actual parse in
    /// `dispatch` only sees bool flags declared by commands on
    /// the resolved path, never flags declared by unrelated siblings.
    fn preliminary_command_path<'a>(&'a self, argv: &[String]) -> Vec<&'a Command> {
        let union_bools = self.global_bool_flag_set();
        let Ok(invocation) = Invocation::parse_in_tree(
            argv.iter().map(String::as_str),
            |flag| self.parses_as_bool(&union_bools, flag),
            |positionals, token| self.names_command(positionals, token),
        ) else {
            return Vec::new();
        };

        let mut path = Vec::new();
        let mut current_cmds = &self.commands;
        for positional in invocation.positionals() {
            let Some(cmd) = current_cmds.get(positional) else {
                break;
            };
            path.push(cmd);
            current_cmds = &cmd.subcommands;
        }
        path
    }

    /// Collect every bool flag declared by any command in `path`. Used to
    /// disambiguate bare `--flag positional` for the actual parse: a flag is
    /// bool only if some command on the resolved path declared it bool. Flags
    /// declared by unrelated commands (siblings, other branches) are ignored
    /// so the parse honors the invoked command's schema.
    fn path_bool_flag_set(path: &[&Command]) -> HashSet<String> {
        let mut set = HashSet::new();
        for command in path {
            if let Some(usage) = &command.usage {
                usage::extract_bool_flag_names(usage, &mut set);
            }
        }
        set
    }

    /// The `Command` references along an already-resolved path (used to union
    /// declared flags and check `handles_dry_run` / `allow_unknown_flags`).
    pub(crate) fn path_command_refs(&self, path: &[String]) -> Vec<&Command> {
        let mut commands = &self.commands;
        let mut out = Vec::with_capacity(path.len());
        for name in path {
            let Some(found) = commands.get(name) else {
                break;
            };
            out.push(found);
            commands = &found.subcommands;
        }
        out
    }

    /// Walk `positionals` down the command tree as far as they name
    /// commands. `None` when the first one names no command (or there is
    /// none).
    pub(crate) fn resolve_command<'tree, 'args>(
        &'tree self,
        positionals: &'args [String],
    ) -> Option<ResolvedCommand<'tree, 'args>> {
        let mut commands = &self.commands;
        let mut current: Option<&Command> = None;
        let mut depth = 0;

        for token in positionals {
            let Some(found) = commands.get(token) else {
                break;
            };
            current = Some(found);
            commands = &found.subcommands;
            depth += 1;
        }

        current.map(|command| ResolvedCommand {
            command,
            path: &positionals[..depth],
            remaining: &positionals[depth..],
        })
    }
}

/// The `EXTRA_ARG` error for `given`, the positionals after the command
/// `scope`, when there are more of them than `arity` allows for its `usage`.
pub(crate) fn extra_arg_error(
    scope: &str,
    usage: &str,
    arity: Arity,
    given: &[String],
) -> CommandError {
    let Arity { required, optional } = arity;
    let extra_list = given[required + optional..]
        .iter()
        .map(|a| format!("{a:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let expected = match optional {
        0 => format!("{required}"),
        _ => format!("{required} to {}", required + optional),
    };
    CommandError::new(
        ErrorCode::EXTRA_ARG,
        format!(
            "unexpected extra argument(s): {extra_list} (`{scope}` takes {expected} positional \
             argument(s); got {})",
            given.len()
        ),
        format!(
            "Nothing was run. Re-invoke matching the usage template `{usage}`, or drop the \
             extra argument(s)."
        ),
    )
}

/// `args` as UTF-8 strings, or the index of the first argument that is not
/// valid UTF-8. Never lossy.
fn utf8_argv(args: impl IntoIterator<Item = OsString>) -> Result<Vec<String>, usize> {
    let mut strings = Vec::new();
    for (index, arg) in args.into_iter().enumerate() {
        let Ok(arg) = arg.into_string() else {
            return Err(index);
        };
        strings.push(arg);
    }
    Ok(strings)
}

pub(crate) fn path_refs(path: &[String]) -> Vec<&str> {
    path.iter().map(String::as_str).collect()
}

fn collect_bool_flags(command: &Command, set: &mut HashSet<String>) {
    if let Some(usage) = &command.usage {
        usage::extract_bool_flag_names(usage, set);
    }
    for sub in command.subcommands.values() {
        collect_bool_flags(sub, set);
    }
}

/// What the validation gates and the handler-result paths read about the
/// resolved command, for one run.
#[derive(Clone, Copy)]
struct Gate<'run> {
    invocation: &'run Invocation,
    /// The run's clock, read when an envelope is built.
    clock: &'run Arc<ClockFn>,
    command: &'run Command,
    path_strs: &'run [&'run str],
    path_cmds: &'run [&'run Command],
}

/// What [`AgentCli::parse_argv`] made of argv.
enum ParsedArgv<'cli> {
    /// A raw command's handler owns argv from `args_start` on.
    Raw {
        handler: &'cli RawCommandHandler,
        args_start: usize,
        /// The framework's tokens before the command's name, parsed, when
        /// the name was not the leading token. `None` when it was: there is
        /// nothing before it to answer for.
        prefix: Option<Invocation>,
    },
    /// argv parsed into a normal invocation.
    Invocation(Invocation),
    /// argv failed to parse and names no raw command, or the framework's own
    /// tokens before a raw command's name failed to parse.
    Failed(ParseInvocationError),
}

/// The `INVALID_FLAG` refusal of `flag`, found before the name of the raw
/// command `name`, which receives only the tokens after its name.
fn not_delivered_error(name: &str, flag: &str) -> CommandError {
    let flag = usage::flag_display(flag);
    CommandError::new(
        ErrorCode::INVALID_FLAG,
        format!(
            "flag {flag} does not reach raw command `{name}`: it receives only the tokens \
             after its name"
        ),
        format!("Drop it, or pass it after `{name}` if that command takes it."),
    )
    .exit_code(ExitCode::USAGE)
}

/// Positionals walked down the command tree: the deepest command they name.
pub(crate) struct ResolvedCommand<'tree, 'args> {
    pub(crate) command: &'tree Command,
    /// The positionals naming `command`, root first.
    pub(crate) path: &'args [String],
    /// The positionals after the command path.
    pub(crate) remaining: &'args [String],
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::{Value, json};

    use crate::envelope::Envelope;
    use crate::test_support::{rich_cli, sample_cli};

    #[tokio::test]
    async fn command_success_returns_envelope() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "status"]).await;

        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["healthy"], Value::Bool(true));
        assert!(
            !envelope.next_actions.is_empty(),
            "expected next actions, got none"
        );
    }

    #[tokio::test]
    async fn unknown_command_returns_error_with_fix() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "bogus"]).await;

        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, ErrorCode::UNKNOWN_COMMAND);
        assert!(!envelope.fix.is_empty(), "expected a non-empty fix");
        assert!(
            !envelope.next_actions.is_empty(),
            "expected next actions, got none"
        );
        // Framework usage errors carry the typed USAGE exit code (2), not the
        // generic error code (1), so agents can distinguish bad invocations.
        assert_eq!(run.exit_code(), ExitCode::USAGE);
    }

    #[tokio::test]
    async fn a_usage_bool_flag_takes_no_value() {
        // `submit --no-git ./plan.html` must NOT consume `./plan.html` as the
        // flag value, because the command's usage declares `--no-git` as bool.
        let cli = AgentCli::new("agplan", "Plan submission tool").command(
            Command::new("submit", "Submit a plan")
                .usage("agplan submit [path] [--title=<title>] [--no-git]")
                .handler(|req| {
                    let path = req.arg(0).unwrap_or("").to_string();
                    let no_git = req.flag_on("no-git");
                    async move {
                        Ok(CommandOutput::new(json!({
                            "path": path,
                            "no_git": no_git?,
                        })))
                    }
                }),
        );

        let run = cli
            .run(["agplan", "submit", "--no-git", "./plan.html"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["path"], json!("./plan.html"));
        assert_eq!(envelope.result["no_git"], json!(true));
    }

    #[tokio::test]
    async fn bool_flags_are_scoped_to_the_resolved_command() {
        // Regression: when two sibling commands declare the same flag name
        // with conflicting semantics (`cmd_a` treats `--format` as bool,
        // `cmd_b` takes it with a value), invoking `cmd_b --format json`
        // must parse `json` as the flag's value, not as a positional. A
        // global-union bool-flag set would incorrectly mark `--format` as
        // bool everywhere and silently corrupt `cmd_b`'s arguments.
        let cli = AgentCli::new("app", "Conflicting flag schemas")
            .command(
                Command::new("cmd_a", "Bool format")
                    .usage("app cmd_a [--format]")
                    .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
            )
            .command(
                Command::new("cmd_b", "Value format")
                    .usage("app cmd_b [--format=<format>]")
                    .handler(|req| {
                        let format = req.flag("format").unwrap_or("").to_string();
                        let positionals: Vec<String> = req.positionals().to_vec();
                        async move {
                            Ok(CommandOutput::new(json!({
                                "format": format,
                                "positionals": positionals,
                            })))
                        }
                    }),
            );

        let run = cli.run(["app", "cmd_b", "--format", "json"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope, got: {:?}", run.envelope());
        };
        assert_eq!(envelope.result["format"], json!("json"));
        // `json` belongs to the flag, not to the remaining positionals.
        assert_eq!(envelope.result["positionals"], json!([] as [String; 0]));
    }

    #[tokio::test]
    async fn the_command_declaring_a_bool_flag_still_takes_no_value() {
        // Counterpart to `bool_flags_are_scoped_to_the_resolved_command`:
        // invoking the sibling that *did* declare `--format` as bool must
        // still treat the next positional as a positional, not a flag value.
        let cli = AgentCli::new("app", "Conflicting flag schemas")
            .command(
                Command::new("cmd_a", "Bool format")
                    .usage("app cmd_a [path] [--format]")
                    .handler(|req| {
                        let format = req.flag("format").map(ToString::to_string);
                        let path = req.arg(0).unwrap_or("").to_string();
                        async move {
                            Ok(CommandOutput::new(json!({
                                "format": format,
                                "path": path,
                            })))
                        }
                    }),
            )
            .command(
                Command::new("cmd_b", "Value format")
                    .usage("app cmd_b [--format=<format>]")
                    .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
            );

        let run = cli
            .run(["app", "cmd_a", "--format", "./payload.json"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope, got: {:?}", run.envelope());
        };
        assert_eq!(envelope.result["format"], json!("true"));
        assert_eq!(envelope.result["path"], json!("./payload.json"));
    }

    #[tokio::test]
    async fn a_value_flag_takes_the_next_token() {
        // `submit ./plan.html --title "My Title"` must capture `My Title` as
        // the title value, not the string "true".
        let cli = AgentCli::new("agplan", "Plan submission tool").command(
            Command::new("submit", "Submit a plan")
                .usage("agplan submit [path] [--title=<title>]")
                .handler(|req| {
                    let title = req.flag("title").unwrap_or("").to_string();
                    async move { Ok(CommandOutput::new(json!({ "title": title }))) }
                }),
        );

        let run = cli
            .run(["agplan", "submit", "./plan.html", "--title", "My Title"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["title"], json!("My Title"));
    }

    #[tokio::test]
    async fn parse_error_carries_usage_exit_code() {
        let cli = rich_cli();
        // A lone `--` is fine; `--=x` is invalid flag syntax → PARSE_ERROR.
        let run = cli.run(["app", "--=oops"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, ErrorCode::PARSE_ERROR);
        assert_eq!(run.exit_code(), ExitCode::USAGE);
    }

    // --- Unknown-flag rejection ---

    #[tokio::test]
    async fn unknown_flag_is_rejected_with_did_you_mean() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("search", "search")
                .usage("app search <query> [--limit <n>]")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({ "count": 0 }))) }),
        );
        let run = cli.run(["app", "search", "apple", "--lmit", "3"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope, got: {}", run.to_json());
        };
        assert_eq!(envelope.error.code, ErrorCode::UNKNOWN_FLAG);
        assert_eq!(envelope.exit_code, ExitCode::USAGE);
        assert!(
            envelope.fix.contains("--limit"),
            "fix must suggest the near-miss: {}",
            envelope.fix
        );
    }

    #[tokio::test]
    async fn reserved_flags_pass_unknown_flag_validation() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("search", "search")
                .usage("app search <query> [--limit <n>]")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({ "count": 0 }))) }),
        );
        let run = cli
            .run([
                "app",
                "search",
                "apple",
                "--limit",
                "3",
                "--quiet",
                "--no-color",
            ])
            .await;
        assert!(run.envelope().ok(), "reserved flags must not be rejected");
    }

    #[tokio::test]
    async fn allow_unknown_flags_opts_out_of_rejection() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("passthrough", "fwd")
                .usage("app passthrough [--whatever <v>]")
                .allow_unknown_flags()
                .handler(|_req| async move { Ok(CommandOutput::new(json!(null))) }),
        );
        let run = cli
            .run(["app", "passthrough", "--totally-unknown", "x"])
            .await;
        assert!(run.envelope().ok());
    }

    #[tokio::test]
    async fn usage_less_command_skips_unknown_flag_validation() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("free", "freeform")
                .handler(|_req| async move { Ok(CommandOutput::new(json!(null))) }),
        );
        let run = cli.run(["app", "free", "--anything", "goes"]).await;
        assert!(run.envelope().ok());
    }

    // --- Dry-run gate ---

    #[tokio::test]
    async fn dry_run_refused_on_unmarked_command() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("delete", "delete a row")
                .usage("app delete <id>")
                .handler(
                    |_req| async move { panic!("handler must not run under refused --dry-run") },
                ),
        );
        let run = cli.run(["app", "delete", "1", "--dry-run"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected DRY_RUN_UNSUPPORTED error, got: {}", run.to_json());
        };
        assert_eq!(envelope.error.code, ErrorCode::DRY_RUN_UNSUPPORTED);
        assert_eq!(envelope.exit_code, ExitCode::USAGE);
        assert!(envelope.fix.contains("Nothing was changed"));
    }

    // --- Handler panics in-process ---

    #[tokio::test]
    #[should_panic(expected = "simulated handler bug")]
    async fn handler_panic_propagates_in_process() {
        // `run()` installs no panic hook: a panicking handler panics the
        // caller. The `HANDLER_PANIC` envelope `run_env` writes instead is
        // covered by tests/panic_hook.rs, in a subprocess.
        let cli = AgentCli::new("app", "x").command(
            Command::new("boom", "boom")
                .usage("app boom")
                .handler(|_req| async move {
                    // Stand in for any unwrap/expect/index panic in handler code.
                    panic!("simulated handler bug")
                }),
        );
        let _ = cli.run(["app", "boom"]).await;
    }

    #[test]
    fn non_utf8_argument_is_a_parse_error() {
        use std::os::unix::ffi::OsStringExt;

        let args = vec![
            OsString::from("wokhei"),
            OsString::from("status"),
            OsString::from_vec(vec![0x66, 0x6f, 0xff]),
        ];
        assert_eq!(utf8_argv(args), Err(2));
        assert_eq!(
            utf8_argv([OsString::from("wokhei"), OsString::from("status")]),
            Ok(vec!["wokhei".to_string(), "status".to_string()])
        );

        let run = sample_cli().non_utf8_argument(2);
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope, got {}", run.to_json());
        };
        assert_eq!(envelope.error.code, ErrorCode::PARSE_ERROR);
        assert_eq!(envelope.error.message, "argument 2 is not valid UTF-8");
        assert_eq!(run.exit_code(), ExitCode::USAGE);
    }

    #[tokio::test]
    async fn declared_short_value_flag_is_accepted() {
        let cli = AgentCli::new("t", "tool").command(
            Command::new("log", "Show log")
                .usage("t log [-n <count>] [--follow]")
                .handler(|req| {
                    let n = req.flag("n").unwrap_or("10").to_string();
                    async move { Ok(CommandOutput::new(json!({ "n": n }))) }
                }),
        );
        let run = cli.run(["t", "log", "-n", "5"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success, got {}", run.to_json());
        };
        assert_eq!(envelope.result["n"], json!("5"));
    }

    #[tokio::test]
    async fn unknown_single_char_flag_renders_single_dash() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "status", "-z"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error");
        };
        assert!(
            envelope.error.message.contains("-z") && !envelope.error.message.contains("--z"),
            "{}",
            envelope.error.message
        );
    }

    // --- Did-you-mean covers reserved flags (R4) ---

    #[tokio::test]
    async fn typod_reserved_flag_gets_did_you_mean() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "status", "--selct=healthy"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error");
        };
        assert_eq!(envelope.error.code, ErrorCode::UNKNOWN_FLAG);
        assert!(
            envelope.fix.contains("Did you mean `--select`?"),
            "{}",
            envelope.fix
        );
    }

    // --- Extra positionals are rejected (R3) ---

    #[tokio::test]
    async fn extra_positional_is_rejected() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "status", "surplus"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error, got {}", run.to_json());
        };
        assert_eq!(envelope.error.code, ErrorCode::EXTRA_ARG);
        assert_eq!(envelope.exit_code, ExitCode::USAGE);
        assert!(
            envelope.error.message.contains("\"surplus\""),
            "{}",
            envelope.error.message
        );
        assert!(envelope.fix.contains("wokhei status"), "{}", envelope.fix);
    }

    #[tokio::test]
    async fn variadic_usage_allows_extra_positionals() {
        let cli = AgentCli::new("t", "tool").command(
            Command::new("run", "Run things")
                .usage("t run <cmd> [args...]")
                .handler(|req| {
                    let count = req.positionals().len();
                    async move { Ok(CommandOutput::new(json!({ "count": count }))) }
                }),
        );
        let run = cli.run(["t", "run", "make", "all", "install"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success, got {}", run.to_json());
        };
        assert_eq!(envelope.result["count"], json!(3));
    }

    #[tokio::test]
    async fn allow_extra_args_opts_out_of_arity_check() {
        let cli = AgentCli::new("t", "tool").command(
            Command::new("echo", "Echo")
                .usage("t echo <first>")
                .allow_extra_args()
                .handler(|req| {
                    let count = req.positionals().len();
                    async move { Ok(CommandOutput::new(json!({ "count": count }))) }
                }),
        );
        let run = cli.run(["t", "echo", "a", "b", "c"]).await;
        assert!(matches!(run.envelope(), Envelope::Success(_)));
    }
}
