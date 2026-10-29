//! The self-documenting command tree: root, help, and version answers.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value, json};

use crate::agent_cli::AgentCli;
use crate::clock::ClockFn;
use crate::codes::{self, ERROR_CODE_DOCS, EXIT_CODE_DOCS, ErrorCode};
use crate::command::Command;
use crate::command_error::CommandError;
use crate::dispatch;
use crate::execution::Execution;
use crate::invocation::Invocation;
use crate::next_action::NextAction;
use crate::reserved_flags::RESERVED_FLAG_DOCS;
use crate::suggest;
use crate::usage::Arity;

impl AgentCli {
    /// The root command tree as doc entries, for the skill renderer.
    pub(crate) fn command_docs(&self) -> Vec<CommandDoc<'_>> {
        let mut path_buf = Vec::new();
        let mut docs = Vec::with_capacity(self.commands.len());
        Self::command_docs_into(&self.name, &mut path_buf, &self.commands, &mut docs);
        docs
    }

    /// The `UNKNOWN_FLAG` answer for a framework-rendered tree (root or
    /// version), or the tokens before a raw command's name, carrying a flag
    /// the framework does not reserve: none of them declare flags of their
    /// own. `known` names flags that are someone's without being the line's,
    /// a raw command's own before its name, so they are not called unknown.
    pub(crate) fn framework_flag_error(
        &self,
        invocation: &Invocation,
        known: &HashSet<String>,
        clock: &ClockFn,
    ) -> Option<Execution> {
        let error =
            self.unknown_flag_error(invocation, &HashSet::new(), known, invocation.program())?;
        Some(self.error_execution(
            invocation.command_line().to_string(),
            clock,
            error.next_actions(self.root_actions()),
        ))
    }

    /// Answer `--version` / `-V`, wherever it sits, when the positionals name
    /// a command or nothing. Flags are checked against the schema of the
    /// command they name, the way running it would check them, or against
    /// the root's when they name none. A first positional that names no
    /// command is `UNKNOWN_COMMAND`, as it is for `--help`: the version is
    /// not an answer to `app nope --version`.
    pub(crate) fn version_flag_execution(
        &self,
        invocation: &Invocation,
        clock: &ClockFn,
    ) -> Execution {
        let Some(resolved) = self.resolve_command(invocation.positionals()) else {
            return match invocation.positionals() {
                [unknown, ..] => self.unknown_command_execution(invocation, clock, unknown),
                [] => self
                    .framework_flag_error(invocation, &HashSet::new(), clock)
                    .unwrap_or_else(|| self.version_answer(invocation, clock)),
            };
        };
        let path_strs = dispatch::path_refs(resolved.path);
        if let Some(declared) = Self::declared_flags(&self.path_command_refs(resolved.path))
            && let Some(error) = self.unknown_flag_error(
                invocation,
                &declared,
                &HashSet::new(),
                &path_strs.join(" "),
            )
        {
            return self.error_execution(
                invocation.command_line().to_string(),
                clock,
                error.next_actions(self.default_command_actions(&path_strs, resolved.command)),
            );
        }
        self.version_answer(invocation, clock)
    }

    /// Answer the `version` positional, the spelling of `--version` for an
    /// agent that guesses a command. It takes no argument: `version extra` is
    /// `EXTRA_ARG` naming `extra`, not the version with `extra` dropped.
    pub(crate) fn version_alias_execution(
        &self,
        invocation: &Invocation,
        clock: &ClockFn,
    ) -> Execution {
        let given = &invocation.positionals()[1..];
        if !given.is_empty() {
            let usage = format!("{} version", self.name);
            let arity = Arity {
                required: 0,
                optional: 0,
            };
            return self.error_execution(
                invocation.command_line().to_string(),
                clock,
                dispatch::extra_arg_error(
                    "version",
                    &usage,
                    arity,
                    &(1..=given.len())
                        .map(|at| invocation.shown_positional(at).to_string())
                        .collect::<Vec<_>>(),
                )
                .next_actions(self.root_actions()),
            );
        }
        // `version` names no command, so the flags are checked as the root's.
        self.framework_flag_error(invocation, &HashSet::new(), clock)
            .unwrap_or_else(|| self.version_answer(invocation, clock))
    }

    /// `{name, version}` instead of the full command tree, `version` omitted
    /// when the CLI set none. The callers have checked the flags.
    fn version_answer(&self, invocation: &Invocation, clock: &ClockFn) -> Execution {
        let mut result = Map::new();
        result.insert("name".to_string(), Value::String(self.name.clone()));
        if let Some(version) = &self.version {
            result.insert("version".to_string(), Value::String(version.clone()));
        }
        self.framework_success(
            invocation,
            clock,
            Value::Object(result),
            vec![NextAction::new(
                self.name.clone(),
                "Inspect the full command tree",
            )],
        )
    }

    pub(crate) fn root_execution(&self, invocation: &Invocation, clock: &ClockFn) -> Execution {
        if let Some(error) = self.framework_flag_error(invocation, &HashSet::new(), clock) {
            return error;
        }
        let result = self.root_result(invocation.program());
        self.framework_success(invocation, clock, result, self.root_actions())
    }

    /// Render help for `targets` (a command path, possibly empty for root
    /// help). Reached via `--help`/`-h` anywhere on the line and via the
    /// `<tool> help [command...]` alias.
    pub(crate) fn help_execution(
        &self,
        invocation: &Invocation,
        clock: &ClockFn,
        targets: &[String],
    ) -> Execution {
        let [first, ..] = targets else {
            return self.root_execution(invocation, clock);
        };

        let offset = invocation.positionals().len() - targets.len();
        let Some(resolved) = self.resolve_command(targets) else {
            let valid: Vec<&str> = self.commands.keys().map(String::as_str).collect();
            return self.error_execution(
                invocation.command_line().to_string(),
                clock,
                CommandError::new(
                    ErrorCode::UNKNOWN_COMMAND,
                    format!("unknown command: {}", invocation.shown_positional(offset)),
                    suggest::unknown_command_fix("command", first, &valid),
                )
                .next_actions(self.root_actions()),
            );
        };
        let command = resolved.command;

        // Reject trailing unknown tokens when the command has subcommands
        if !resolved.remaining.is_empty() && !command.subcommands.is_empty() {
            let path_strs = dispatch::path_refs(resolved.path);
            let valid: Vec<&str> = command.subcommands.keys().map(String::as_str).collect();
            return self.error_execution(
                invocation.command_line().to_string(),
                clock,
                CommandError::new(
                    ErrorCode::UNKNOWN_SUBCOMMAND,
                    format!(
                        "unknown subcommand: {}",
                        invocation.shown_positional(offset + resolved.path.len())
                    ),
                    suggest::unknown_command_fix("subcommand", &resolved.remaining[0], &valid),
                )
                .next_actions(self.subcommand_actions(&path_strs, command)),
            );
        }

        self.command_tree_execution(invocation, clock, resolved.path, command)
    }

    /// The command tree of `command` (at `path`): its usage and its
    /// subcommands. Flags are checked against the command's own schema, the
    /// way running it would check them, so `calc add --lmit --help` fails as
    /// `calc add --lmit` would.
    pub(crate) fn command_tree_execution(
        &self,
        invocation: &Invocation,
        clock: &ClockFn,
        path: &[String],
        command: &Command,
    ) -> Execution {
        let path_strs: Vec<&str> = path.iter().map(String::as_str).collect();
        if let Some(declared) = Self::declared_flags(&self.path_command_refs(path))
            && let Some(error) = self.unknown_flag_error(
                invocation,
                &declared,
                &HashSet::new(),
                &path_strs.join(" "),
            )
        {
            return self.error_execution(
                invocation.command_line().to_string(),
                clock,
                error.next_actions(self.subcommand_actions(&path_strs, command)),
            );
        }

        let usage = command.usage_or_default(invocation.program(), &path_strs);
        // Seed the path buffer with the resolved path so default usages render
        // the full `program group sub` form.
        let mut path_buf: Vec<&str> = path_strs.clone();
        let mut subcommands = Vec::with_capacity(command.subcommands.len());
        Self::command_docs_into(
            invocation.program(),
            &mut path_buf,
            &command.subcommands,
            &mut subcommands,
        );
        let mut result = Map::new();
        result.insert("name".to_string(), Value::String(command.name.clone()));
        result.insert(
            "description".to_string(),
            Value::String(command.description.clone()),
        );
        result.insert("usage".to_string(), Value::String(usage));
        result.insert("subcommands".to_string(), docs_array(&subcommands));
        if command.is_stream() {
            result.insert("stream".to_string(), Value::Bool(true));
        }

        self.framework_success(
            invocation,
            clock,
            Value::Object(result),
            self.subcommand_actions(&path_strs, command),
        )
    }

    fn root_result(&self, program: &str) -> Value {
        let mut path_buf = Vec::new();
        let mut docs = Vec::with_capacity(self.commands.len());
        Self::command_docs_into(program, &mut path_buf, &self.commands, &mut docs);
        let mut result = Map::new();

        // Insert user-provided extras first so core keys always win
        result.extend(self.root_extra.clone());

        result.insert(
            "description".to_string(),
            Value::String(self.description.clone()),
        );
        if let Some(version) = &self.version {
            result.insert("version".to_string(), Value::String(version.clone()));
        }
        result.insert("commands".to_string(), docs_array(&docs));

        // Surface the reserved agent-native flags in the self-documenting tree
        // so an introspecting agent can discover the whole surface it can drive
        // (these are honored on every command but declared by none).
        if self.reserved_flags {
            let flags: Vec<Value> = RESERVED_FLAG_DOCS
                .iter()
                .map(|(flag, description)| json!({ "flag": flag, "description": description }))
                .collect();
            result.insert("agent_flags".to_string(), Value::Array(flags));
        }

        // Publish the exit-code and error-code dictionaries. Both come from
        // the same tables the generated skill renders, so the JSON tree and
        // the markdown can never disagree.
        result.insert(
            "exit_codes".to_string(),
            codes::docs_object(
                EXIT_CODE_DOCS
                    .iter()
                    .map(|(code, description)| (code.to_string(), *description)),
            ),
        );
        result.insert(
            "error_codes".to_string(),
            codes::docs_object(
                ERROR_CODE_DOCS
                    .iter()
                    .map(|(code, description)| (code.as_str().to_string(), *description)),
            ),
        );

        Value::Object(result)
    }

    /// Build command docs using push/pop pattern to avoid quadratic cloning.
    fn command_docs_into<'a>(
        program: &str,
        path_buf: &mut Vec<&'a str>,
        commands: &'a BTreeMap<String, Command>,
        out: &mut Vec<CommandDoc<'a>>,
    ) {
        for command in commands.values() {
            path_buf.push(&command.name);
            let usage = command.usage_or_default(program, path_buf);
            let mut sub_docs = Vec::with_capacity(command.subcommands.len());
            Self::command_docs_into(program, path_buf, &command.subcommands, &mut sub_docs);
            out.push(CommandDoc {
                name: &command.name,
                description: &command.description,
                usage,
                raw: command.is_raw(),
                stream: command.is_stream(),
                subcommands: sub_docs,
            });
            path_buf.pop();
        }
    }
}

/// `docs` as the JSON array the root tree and help publish.
fn docs_array(docs: &[CommandDoc<'_>]) -> Value {
    Value::Array(docs.iter().map(CommandDoc::to_value).collect())
}

/// One command of the self-documenting tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandDoc<'tree> {
    name: &'tree str,
    pub(crate) description: &'tree str,
    pub(crate) usage: String,
    /// True only for [`Command::raw_handler`] commands, so an introspecting
    /// agent knows this one answers with raw stdout and its own exit code
    /// instead of a JSON envelope.
    pub(crate) raw: bool,
    /// True only for [`Command::stream_handler`] commands, so an
    /// introspecting agent knows this one answers with NDJSON event lines and
    /// one terminal envelope line.
    pub(crate) stream: bool,
    pub(crate) subcommands: Vec<CommandDoc<'tree>>,
}

impl CommandDoc<'_> {
    /// The doc as a JSON object: `name`, `description`, `usage`, then `raw`
    /// and `stream` only when true and `subcommands` only when there are
    /// any. Built directly, so it cannot fail.
    fn to_value(&self) -> Value {
        let mut doc = Map::new();
        doc.insert("name".to_string(), Value::String(self.name.to_string()));
        doc.insert(
            "description".to_string(),
            Value::String(self.description.to_string()),
        );
        doc.insert("usage".to_string(), Value::String(self.usage.clone()));
        if self.raw {
            doc.insert("raw".to_string(), Value::Bool(true));
        }
        if self.stream {
            doc.insert("stream".to_string(), Value::Bool(true));
        }
        if !self.subcommands.is_empty() {
            doc.insert("subcommands".to_string(), docs_array(&self.subcommands));
        }
        Value::Object(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::codes::ExitCode;
    use crate::command_output::CommandOutput;
    use crate::envelope::Envelope;
    use crate::test_support::{rich_cli, sample_cli};

    #[tokio::test]
    async fn root_returns_self_documenting_json_tree() {
        let cli = sample_cli();
        let run = cli.run(["wokhei"]).await;

        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.result["commands"].is_array());
        assert!(
            !envelope.next_actions.is_empty(),
            "expected next actions, got none"
        );
        assert_eq!(run.exit_code(), ExitCode::SUCCESS);
    }

    #[tokio::test]
    async fn help_returns_json_not_plain_text() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "gateway", "--help"]).await;

        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(
            envelope.result["name"],
            Value::String("gateway".to_string())
        );
        assert!(envelope.result["subcommands"].is_array());
    }

    // --- Help with trailing unknown tokens ---

    #[tokio::test]
    async fn help_with_trailing_unknown_returns_error() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "gateway", "bogus", "--help"]).await;

        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, ErrorCode::UNKNOWN_SUBCOMMAND);
    }

    // --- root_field cannot shadow core keys ---

    #[tokio::test]
    async fn root_field_cannot_shadow_core_keys() {
        let cli = AgentCli::new("test", "Test CLI")
            .version("1.0.0")
            .root_field("description", json!("hacked"))
            .root_field("version", json!("hacked"))
            .root_field("commands", json!("hacked"));

        let run = cli.run(["test"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        // Core keys must not be overwritten by root_field
        assert_eq!(envelope.result["description"], json!("Test CLI"));
        assert_eq!(envelope.result["version"], json!("1.0.0"));
        assert!(envelope.result["commands"].is_array());
    }

    // --- help alias + --version ---

    #[tokio::test]
    async fn help_alias_routes_to_command_help() {
        let cli = rich_cli();
        let run = cli.run(["app", "help", "get"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope, got: {}", run.to_json());
        };
        assert_eq!(envelope.result["name"], json!("get"));
        assert_eq!(envelope.result["usage"], json!("app get <id>"));
    }

    #[tokio::test]
    async fn bare_help_alias_shows_root() {
        let cli = rich_cli();
        let run = cli.run(["app", "help"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.result.get("commands").is_some());
    }

    #[tokio::test]
    async fn bare_version_flag_returns_version_only() {
        let cli = rich_cli().version("9.9.9");
        let run = cli.run(["app", "--version"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(
            envelope.result,
            json!({ "name": "app", "version": "9.9.9" })
        );
    }

    #[tokio::test]
    async fn version_is_omitted_when_none_is_set() {
        let run = rich_cli().run(["app", "--version"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope, got {}", run.to_json());
        };
        assert_eq!(envelope.result, json!({ "name": "app" }));
    }

    #[tokio::test]
    async fn root_commands_are_built_as_plain_json() {
        let cli = AgentCli::new("app", "x")
            .command(
                Command::new("grep", "Search")
                    .usage("app grep [args...]")
                    .raw_handler(|_args| async move { ExitCode::SUCCESS }),
            )
            .command(
                Command::new("deploy", "Deploy")
                    .usage("app deploy")
                    .stream_handler(|_req, _out| async move { Ok(CommandOutput::new(json!({}))) }),
            )
            .command(
                Command::new("food", "Food").subcommand(
                    Command::new("search", "Search foods")
                        .usage("app food search <query>")
                        .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
                ),
            );
        let run = cli.run(["app"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(
            envelope.result["commands"],
            json!([
                { "name": "deploy", "description": "Deploy", "usage": "app deploy", "stream": true },
                {
                    "name": "food",
                    "description": "Food",
                    "usage": "app food <subcommand>",
                    "subcommands": [
                        { "name": "search", "description": "Search foods", "usage": "app food search <query>" }
                    ]
                },
                { "name": "grep", "description": "Search", "usage": "app grep [args...]", "raw": true }
            ])
        );
    }

    // --- group help carries real subcommand docs ---

    #[tokio::test]
    async fn group_help_lists_subcommand_docs() {
        let cli =
            AgentCli::new("app", "x").command(Command::new("food", "food group").subcommand(
                Command::new("search", "search foods").usage("app food search <query>"),
            ));
        let run = cli.run(["app", "food"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        let subs = envelope.result["subcommands"]
            .as_array()
            .expect("subcommands array");
        assert_eq!(subs.len(), 1, "subcommands must not be empty: {subs:?}");
        assert_eq!(subs[0]["name"], json!("search"));
    }

    // --- root help publishes exit-code dictionary ---

    #[tokio::test]
    async fn root_help_includes_exit_code_dictionary() {
        let cli = rich_cli();
        let run = cli.run(["app"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(
            envelope.result["exit_codes"]["2"]
                .as_str()
                .map(|s| s.starts_with("usage")),
            Some(true)
        );
    }

    // --- Reserved flags discoverable from the root tree ---

    #[tokio::test]
    async fn root_tree_lists_agent_flags() {
        let cli = rich_cli();
        let run = cli.run(["app"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        let flags = envelope.result["agent_flags"]
            .as_array()
            .expect("agent_flags array");
        assert!(
            flags
                .iter()
                .any(|f| f["flag"].as_str().unwrap_or("").starts_with("--select"))
        );
    }

    #[tokio::test]
    async fn agent_flags_absent_when_reserved_flags_disabled() {
        let cli = rich_cli().without_reserved_flags();
        let run = cli.run(["app"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.result.get("agent_flags").is_none());
    }

    // --- Framework paths honor the reserved output flags (R5) ---

    #[tokio::test]
    async fn root_honors_quiet_and_select() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "--quiet"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success");
        };
        assert!(
            envelope.next_actions.is_empty(),
            "expected no next actions, got {:?}",
            envelope.next_actions
        );

        let run = cli.run(["wokhei", "--select=commands"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success");
        };
        let keys: Vec<&String> = envelope
            .result
            .as_object()
            .expect("object")
            .keys()
            .collect();
        assert_eq!(keys, ["commands"], "{keys:?}");
    }

    #[tokio::test]
    async fn help_tree_honors_quiet() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "help", "gateway", "--quiet"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success");
        };
        assert!(
            envelope.next_actions.is_empty(),
            "expected no next actions, got {:?}",
            envelope.next_actions
        );
    }

    // --- Bare `version` positional (R6) ---

    #[tokio::test]
    async fn bare_version_positional_answers_like_version_flag() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "version"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success, got {}", run.to_json());
        };
        assert_eq!(envelope.result["name"], json!("wokhei"));
        assert_eq!(envelope.result["version"], json!("0.1.0"));
    }

    #[tokio::test]
    async fn user_defined_version_command_is_not_shadowed() {
        let cli = AgentCli::new("t", "tool").command(
            Command::new("version", "Custom version info")
                .usage("t version")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({ "custom": true }))) }),
        );
        let run = cli.run(["t", "version"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success");
        };
        assert_eq!(envelope.result["custom"], json!(true));
    }

    #[tokio::test]
    async fn user_defined_help_command_is_not_shadowed() {
        let cli = AgentCli::new("t", "tool").command(
            Command::new("help", "Custom help")
                .usage("t help [<topic>]")
                .handler(
                    |req| async move { Ok(CommandOutput::new(json!({ "topic": req.arg(0) }))) },
                ),
        );
        let run = cli.run(["t", "help", "auth"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success, got {}", run.to_json());
        };
        assert_eq!(envelope.result, json!({ "topic": "auth" }));

        // The flags still answer the framework's help.
        let run = cli.run(["t", "--help"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success, got {}", run.to_json());
        };
        assert!(envelope.result["commands"].is_array(), "{}", run.to_json());
    }

    // --- error_codes dictionary in the root tree (R9) ---

    #[tokio::test]
    async fn root_tree_publishes_error_code_dictionary() {
        let cli = sample_cli();
        let run = cli.run(["wokhei"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success");
        };
        let codes = envelope.result["error_codes"]
            .as_object()
            .expect("error_codes object");
        for expected in [
            "UNKNOWN_COMMAND",
            "UNKNOWN_FLAG",
            "EXTRA_ARG",
            "MISSING_ARG",
            "INVALID_ARG",
            "DRY_RUN_UNSUPPORTED",
            "HANDLER_PANIC",
        ] {
            assert!(codes.contains_key(expected), "missing {expected}");
        }
    }
}
