use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::envelope::{Envelope, ErrorEnvelope, NextAction, SuccessEnvelope};

/// Maximum recursion depth for command tree traversal.
const MAX_COMMAND_DEPTH: usize = 32;

/// Parsed command-line invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    program: String,
    command_line: String,
    raw_args: Vec<String>,
    flags: HashMap<String, String>,
    positionals: Vec<String>,
    help_requested: bool,
}

impl Invocation {
    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn command_line(&self) -> &str {
        &self.command_line
    }

    /// Consume the invocation and return the owned command_line string.
    pub fn into_command_line(self) -> String {
        self.command_line
    }

    pub fn raw_args(&self) -> &[String] {
        &self.raw_args
    }

    pub fn flags(&self) -> &HashMap<String, String> {
        &self.flags
    }

    pub fn flag(&self, name: &str) -> Option<&str> {
        self.flags.get(name).map(String::as_str)
    }

    pub fn positionals(&self) -> &[String] {
        &self.positionals
    }

    pub fn help_requested(&self) -> bool {
        self.help_requested
    }
}

/// Invocation parser failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseInvocationError {
    EmptyArgv,
    InvalidFlag(String),
}

impl fmt::Display for ParseInvocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyArgv => write!(f, "argv is empty"),
            Self::InvalidFlag(flag) => write!(f, "invalid flag syntax: {flag}"),
        }
    }
}

/// Returns true if `s` looks like a negative number (integer or float).
fn looks_like_negative_number(s: &str) -> bool {
    if !s.starts_with('-') || s.len() < 2 {
        return false;
    }
    let rest = &s[1..];
    let mut seen_dot = false;
    for (i, ch) in rest.chars().enumerate() {
        if ch == '.' && !seen_dot && i > 0 {
            seen_dot = true;
        } else if !ch.is_ascii_digit() {
            return false;
        }
    }
    // Must have at least one digit
    rest.chars().any(|c| c.is_ascii_digit())
}

/// Parse argv into an `Invocation`.
pub fn parse_invocation<I, S>(args: I) -> Result<Invocation, ParseInvocationError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut iter = args.into_iter().map(Into::into);
    let raw_program = iter.next().ok_or(ParseInvocationError::EmptyArgv)?;
    let program = Path::new(&raw_program)
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToString::to_string)
        .unwrap_or(raw_program);
    let tokens: Vec<String> = iter.collect();
    let command_line = if tokens.is_empty() {
        program.clone()
    } else {
        format!("{program} {}", tokens.join(" "))
    };

    let mut flags = HashMap::new();
    let mut positionals = Vec::new();
    let mut help_requested = false;
    let mut positional_only = false;

    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];

        if positional_only {
            positionals.push(token.clone());
            i += 1;
            continue;
        }

        if token == "--" {
            positional_only = true;
            i += 1;
            continue;
        }

        if token == "--help" || token == "-h" {
            help_requested = true;
            i += 1;
            continue;
        }

        // Long flags: --key or --key=value
        if let Some(flag) = token.strip_prefix("--") {
            if flag.is_empty() {
                return Err(ParseInvocationError::InvalidFlag(token.clone()));
            }

            if let Some((key, value)) = flag.split_once('=') {
                if key.is_empty() {
                    return Err(ParseInvocationError::InvalidFlag(token.clone()));
                }
                flags.insert(key.to_string(), value.to_string());
                i += 1;
                continue;
            }

            // Consume next token as value unless it starts with '--'
            // (single-dash values like -123 are allowed as flag values)
            if let Some(next) = tokens.get(i + 1) {
                let is_value = !next.starts_with('-') || looks_like_negative_number(next);
                if is_value {
                    flags.insert(flag.to_string(), next.clone());
                    i += 2;
                    continue;
                }
            }
            flags.insert(flag.to_string(), "true".to_string());
            i += 1;
            continue;
        }

        // Negative numbers as positionals: -123, -3.14
        if looks_like_negative_number(token) {
            positionals.push(token.clone());
            i += 1;
            continue;
        }

        // Short flags: -x or -abc (bundled)
        if token.starts_with('-') && token.len() > 1 {
            let short = &token[1..];
            if short.chars().count() == 1 {
                // Single short flag: -x
                // Consume next token as value unless it starts with '--'
                // (allow negative numbers as values)
                if let Some(next) = tokens.get(i + 1)
                    && (!next.starts_with('-') || looks_like_negative_number(next))
                {
                    flags.insert(short.to_string(), next.clone());
                    i += 2;
                    continue;
                }
                flags.insert(short.to_string(), "true".to_string());
                i += 1;
                continue;
            }

            for ch in short.chars() {
                flags.insert(ch.to_string(), "true".to_string());
            }
            i += 1;
            continue;
        }

        positionals.push(token.clone());
        i += 1;
    }

    Ok(Invocation {
        program,
        command_line,
        raw_args: tokens,
        flags,
        positionals,
        help_requested,
    })
}

/// Mutable state shared across command invocations.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecutionContext {
    memory: HashMap<String, Value>,
}

impl ExecutionContext {
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.memory.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.memory.get(key)
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.memory.remove(key)
    }

    pub fn memory(&self) -> &HashMap<String, Value> {
        &self.memory
    }
}

/// Runtime request passed to each command handler.
pub struct CommandRequest<'a> {
    invocation: &'a Invocation,
    command_path: &'a [String],
    positionals: &'a [String],
}

impl<'a> CommandRequest<'a> {
    pub fn invocation(&self) -> &Invocation {
        self.invocation
    }

    pub fn command_path(&self) -> &[String] {
        self.command_path
    }

    pub fn positionals(&self) -> &[String] {
        self.positionals
    }

    pub fn arg(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(String::as_str)
    }

    pub fn flag(&self, key: &str) -> Option<&str> {
        self.invocation.flag(key)
    }

    pub fn prompt(&self) -> Option<Cow<'_, str>> {
        if let Some(prompt) = self.flag("prompt") {
            return Some(Cow::Borrowed(prompt));
        }
        if self.positionals.is_empty() {
            return None;
        }
        Some(Cow::Owned(self.positionals.join(" ")))
    }
}

/// Success payload returned from a command handler.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandOutput {
    result: Value,
    next_actions: Vec<NextAction>,
}

impl CommandOutput {
    pub fn new(result: Value) -> Self {
        Self {
            result,
            next_actions: Vec::new(),
        }
    }

    pub fn from_serializable<T>(value: T) -> Result<Self, serde_json::Error>
    where
        T: Serialize,
    {
        Ok(Self::new(serde_json::to_value(value)?))
    }

    pub fn next_action(mut self, action: NextAction) -> Self {
        self.next_actions.push(action);
        self
    }

    pub fn next_actions(mut self, actions: Vec<NextAction>) -> Self {
        self.next_actions.extend(actions);
        self
    }
}

/// Error payload returned from a command handler.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandError {
    pub message: String,
    pub code: String,
    pub fix: String,
    pub next_actions: Vec<NextAction>,
}

impl CommandError {
    pub fn new(
        message: impl Into<String>,
        code: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            fix: fix.into(),
            next_actions: Vec::new(),
        }
    }

    pub fn next_action(mut self, action: NextAction) -> Self {
        self.next_actions.push(action);
        self
    }

    pub fn next_actions(mut self, actions: Vec<NextAction>) -> Self {
        self.next_actions.extend(actions);
        self
    }
}

type CommandHandler = dyn Fn(&CommandRequest<'_>, &mut ExecutionContext) -> Result<CommandOutput, CommandError>
    + Send
    + Sync;

/// CLI command definition.
#[derive(Clone)]
pub struct Command {
    name: String,
    description: String,
    usage: Option<String>,
    handler: Option<Arc<CommandHandler>>,
    default_next_actions: Vec<NextAction>,
    subcommands: BTreeMap<String, Command>,
}

use std::collections::BTreeMap;

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("usage", &self.usage)
            .field("default_next_actions", &self.default_next_actions)
            .field("subcommand_count", &self.subcommands.len())
            .field("has_handler", &self.handler.is_some())
            .finish()
    }
}

impl Command {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            usage: None,
            handler: None,
            default_next_actions: Vec::new(),
            subcommands: BTreeMap::new(),
        }
    }

    pub fn usage(mut self, usage: impl Into<String>) -> Self {
        self.usage = Some(usage.into());
        self
    }

    pub fn handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&CommandRequest<'_>, &mut ExecutionContext) -> Result<CommandOutput, CommandError>
            + Send
            + Sync
            + 'static,
    {
        self.handler = Some(Arc::new(handler));
        self
    }

    pub fn subcommand(mut self, command: Command) -> Self {
        self.subcommands.insert(command.name.clone(), command);
        self
    }

    pub fn default_next_action(mut self, action: NextAction) -> Self {
        self.default_next_actions.push(action);
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    fn usage_or_default(&self, program: &str, path: &[&str]) -> String {
        if let Some(usage) = &self.usage {
            return usage.clone();
        }
        let joined = path.join(" ");
        if !self.subcommands.is_empty() && self.handler.is_none() {
            format!("{program} {joined} <subcommand>")
        } else {
            format!("{program} {joined} [--flag <value>] [args...]")
        }
    }
}

/// Executed CLI result wrapper.
#[derive(Clone, Debug, PartialEq)]
pub struct Execution {
    envelope: Envelope,
}

impl Execution {
    pub fn envelope(&self) -> &Envelope {
        &self.envelope
    }

    pub fn exit_code(&self) -> i32 {
        self.envelope.exit_code()
    }

    pub fn to_json(&self) -> String {
        self.envelope.to_json()
    }

    pub fn to_json_pretty(&self) -> String {
        self.envelope.to_json_pretty()
    }
}

/// Agent-native CLI runtime.
#[derive(Clone)]
pub struct AgentCli {
    name: String,
    description: String,
    version: Option<String>,
    commands: BTreeMap<String, Command>,
    root_extra: Map<String, Value>,
}

impl fmt::Debug for AgentCli {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentCli")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("version", &self.version)
            .field("command_count", &self.commands.len())
            .finish()
    }
}

impl AgentCli {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            version: None,
            commands: BTreeMap::new(),
            root_extra: Map::new(),
        }
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn command(mut self, command: Command) -> Self {
        self.commands.insert(command.name.clone(), command);
        self
    }

    pub fn root_field(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.root_extra.insert(key.into(), value.into());
        self
    }

    pub fn run_env(&self) -> Execution {
        let mut context = ExecutionContext::default();
        self.run_argv_with_context(std::env::args(), &mut context)
    }

    pub fn run_env_with_context(&self, context: &mut ExecutionContext) -> Execution {
        self.run_argv_with_context(std::env::args(), context)
    }

    pub fn run_argv<I, S>(&self, args: I) -> Execution
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut context = ExecutionContext::default();
        self.run_argv_with_context(args, &mut context)
    }

    pub fn run_argv_with_context<I, S>(&self, args: I, context: &mut ExecutionContext) -> Execution
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut argv: Vec<String> = args.into_iter().map(Into::into).collect();
        if argv.is_empty() {
            argv.push(self.name.clone());
        }

        let invocation = match parse_invocation(argv.iter().map(String::as_str)) {
            Ok(value) => value,
            Err(error) => {
                // Defer fallback_command construction to the error path
                let fallback_command = argv.join(" ");
                return self.error_execution(
                    fallback_command,
                    error.to_string(),
                    "PARSE_ERROR",
                    "Use valid CLI syntax. Run the root command to inspect command templates.",
                    self.root_actions(),
                );
            }
        };

        if invocation.help_requested() {
            return self.help_execution(&invocation);
        }

        if invocation.positionals().is_empty() {
            return self.root_execution(&invocation);
        }

        let resolved = self.resolve_command(invocation.positionals());
        if resolved.path.is_empty() {
            let unknown = invocation
                .positionals()
                .first()
                .cloned()
                .unwrap_or_else(|| "<missing>".to_string());
            return self.error_execution(
                invocation.command_line().to_string(),
                format!("unknown command: {unknown}"),
                "UNKNOWN_COMMAND",
                "Run the root command and use one of the listed command templates.",
                self.root_actions(),
            );
        }

        let command = match resolved.command {
            Some(value) => value,
            None => {
                return self.error_execution(
                    invocation.command_line().to_string(),
                    "unknown command".to_string(),
                    "UNKNOWN_COMMAND",
                    "Run the root command and use one of the listed command templates.",
                    self.root_actions(),
                );
            }
        };

        if command.handler.is_none() && !command.subcommands.is_empty() {
            if resolved.remaining.is_empty() {
                return self.command_tree_execution(&invocation, &resolved.path, command);
            }
            let path_strs: Vec<&str> = resolved.path.iter().map(String::as_str).collect();
            return self.error_execution(
                invocation.command_line().to_string(),
                format!("unknown subcommand: {}", resolved.remaining[0]),
                "UNKNOWN_SUBCOMMAND",
                format!(
                    "Use one of the listed subcommands under `{}`.",
                    resolved.path.join(" ")
                ),
                self.subcommand_actions(&path_strs, command),
            );
        }

        let handler = match &command.handler {
            Some(value) => value,
            None => {
                return self.error_execution(
                    invocation.command_line().to_string(),
                    "command has no handler".to_string(),
                    "MISSING_HANDLER",
                    "Attach a handler for this command or route to a subcommand.",
                    self.root_actions(),
                );
            }
        };

        let request = CommandRequest {
            invocation: &invocation,
            command_path: &resolved.path,
            positionals: resolved.remaining,
        };

        let path_strs: Vec<&str> = resolved.path.iter().map(String::as_str).collect();
        match handler(&request, context) {
            Ok(output) => {
                let mut next_actions = output.next_actions;
                if next_actions.is_empty() {
                    next_actions = self.default_command_actions(&path_strs, command);
                }
                Execution {
                    envelope: SuccessEnvelope::new(
                        invocation.into_command_line(),
                        output.result,
                        next_actions,
                    )
                    .into(),
                }
            }
            Err(error) => {
                let mut next_actions = error.next_actions;
                if next_actions.is_empty() {
                    next_actions = self.default_command_actions(&path_strs, command);
                }
                self.error_execution(
                    invocation.into_command_line(),
                    error.message,
                    error.code,
                    error.fix,
                    next_actions,
                )
            }
        }
    }

    fn root_execution(&self, invocation: &Invocation) -> Execution {
        let result = self.root_result(invocation.program());
        Execution {
            envelope: SuccessEnvelope::new(
                invocation.command_line().to_string(),
                result,
                self.root_actions(),
            )
            .into(),
        }
    }

    fn help_execution(&self, invocation: &Invocation) -> Execution {
        if invocation.positionals().is_empty() {
            return self.root_execution(invocation);
        }

        let resolved = self.resolve_command(invocation.positionals());
        if resolved.path.is_empty() {
            return self.error_execution(
                invocation.command_line().to_string(),
                format!("unknown command: {}", invocation.positionals()[0]),
                "UNKNOWN_COMMAND",
                "Run the root command and inspect the listed templates.",
                self.root_actions(),
            );
        }

        let command = match resolved.command {
            Some(value) => value,
            None => {
                return self.error_execution(
                    invocation.command_line().to_string(),
                    "unknown command".to_string(),
                    "UNKNOWN_COMMAND",
                    "Run the root command and inspect the listed templates.",
                    self.root_actions(),
                );
            }
        };

        self.command_tree_execution(invocation, &resolved.path, command)
    }

    fn command_tree_execution(
        &self,
        invocation: &Invocation,
        path: &[String],
        command: &Command,
    ) -> Execution {
        let path_strs: Vec<&str> = path.iter().map(String::as_str).collect();
        let usage = command.usage_or_default(invocation.program(), &path_strs);
        let mut buf = Vec::new();
        self.command_docs_recursive(invocation.program(), &mut buf, &command.subcommands, 0);
        let subcommands = buf;
        let result = json!({
            "name": command.name(),
            "description": command.description(),
            "usage": usage,
            "subcommands": subcommands
        });

        Execution {
            envelope: SuccessEnvelope::new(
                invocation.command_line().to_string(),
                result,
                self.subcommand_actions(&path_strs, command),
            )
            .into(),
        }
    }

    fn root_result(&self, program: &str) -> Value {
        let mut path_buf = Vec::new();
        let mut docs = Vec::with_capacity(self.commands.len());
        self.command_docs_into(program, &mut path_buf, &self.commands, &mut docs, 0);
        let mut result = Map::new();
        result.insert(
            "description".to_string(),
            Value::String(self.description.clone()),
        );
        if let Some(version) = &self.version {
            result.insert("version".to_string(), Value::String(version.clone()));
        }
        result.insert(
            "commands".to_string(),
            serde_json::to_value(docs)
                .unwrap_or_else(|e| Value::String(format!("serialization failed: {e}"))),
        );

        for (key, value) in &self.root_extra {
            result.insert(key.clone(), value.clone());
        }

        Value::Object(result)
    }

    fn root_actions(&self) -> Vec<NextAction> {
        if self.commands.is_empty() {
            return vec![NextAction::new(
                self.name.clone(),
                "Inspect root command tree",
            )];
        }

        let mut actions = Vec::with_capacity(self.commands.len());
        for command in self.commands.values() {
            let path = [command.name.as_str()];
            let usage = command.usage_or_default(&self.name, &path);
            actions.push(NextAction::new(usage, command.description.clone()));
        }
        actions
    }

    fn default_command_actions(&self, path: &[&str], command: &Command) -> Vec<NextAction> {
        if !command.default_next_actions.is_empty() {
            return command.default_next_actions.clone();
        }

        let usage = command.usage_or_default(&self.name, path);
        vec![
            NextAction::new(usage, "Run this command template"),
            NextAction::new(self.name.clone(), "Inspect the full command tree"),
        ]
    }

    fn subcommand_actions(&self, path: &[&str], command: &Command) -> Vec<NextAction> {
        if command.subcommands.is_empty() {
            return self.default_command_actions(path, command);
        }

        let mut actions = Vec::with_capacity(command.subcommands.len() + 1);
        for sub in command.subcommands.values() {
            let mut sub_path: Vec<&str> = path.to_vec();
            sub_path.push(&sub.name);
            let usage = sub.usage_or_default(&self.name, &sub_path);
            actions.push(NextAction::new(usage, sub.description.clone()));
        }
        actions.push(NextAction::new(
            self.name.clone(),
            "Inspect the full command tree",
        ));
        actions
    }

    fn error_execution(
        &self,
        command: String,
        message: impl Into<String>,
        code: impl Into<String>,
        fix: impl Into<String>,
        next_actions: Vec<NextAction>,
    ) -> Execution {
        Execution {
            envelope: ErrorEnvelope::new(command, message, code, fix, next_actions).into(),
        }
    }

    /// Build command docs using push/pop pattern to avoid quadratic cloning.
    fn command_docs_into<'a>(
        &self,
        program: &str,
        path_buf: &mut Vec<&'a str>,
        commands: &'a BTreeMap<String, Command>,
        out: &mut Vec<CommandDoc>,
        depth: usize,
    ) {
        if depth >= MAX_COMMAND_DEPTH {
            return;
        }
        for command in commands.values() {
            path_buf.push(&command.name);
            let usage = command.usage_or_default(program, path_buf);
            let mut sub_docs = Vec::with_capacity(command.subcommands.len());
            self.command_docs_into(
                program,
                path_buf,
                &command.subcommands,
                &mut sub_docs,
                depth + 1,
            );
            out.push(CommandDoc {
                name: command.name.clone(),
                description: command.description.clone(),
                usage,
                subcommands: sub_docs,
            });
            path_buf.pop();
        }
    }

    /// Helper for command_tree_execution that starts a fresh path buffer.
    fn command_docs_recursive<'a>(
        &self,
        program: &str,
        path_buf: &mut Vec<&'a str>,
        commands: &'a BTreeMap<String, Command>,
        depth: usize,
    ) -> Vec<CommandDoc> {
        if depth >= MAX_COMMAND_DEPTH {
            return Vec::new();
        }
        let mut docs = Vec::with_capacity(commands.len());
        for command in commands.values() {
            path_buf.push(&command.name);
            let usage = command.usage_or_default(program, path_buf);
            let sub_docs =
                self.command_docs_recursive(program, path_buf, &command.subcommands, depth + 1);
            docs.push(CommandDoc {
                name: command.name.clone(),
                description: command.description.clone(),
                usage,
                subcommands: sub_docs,
            });
            path_buf.pop();
        }
        docs
    }

    fn resolve_command<'a>(&'a self, positionals: &'a [String]) -> ResolvedCommand<'a> {
        let mut commands = &self.commands;
        let mut consumed = 0;
        let mut path = Vec::new();
        let mut current: Option<&Command> = None;

        while consumed < positionals.len() {
            let token = &positionals[consumed];
            let Some(found) = commands.get(token) else {
                break;
            };
            path.push(token.clone());
            consumed += 1;
            current = Some(found);
            commands = &found.subcommands;
        }

        ResolvedCommand {
            command: current,
            path,
            remaining: &positionals[consumed..],
        }
    }
}

struct ResolvedCommand<'a> {
    command: Option<&'a Command>,
    path: Vec<String>,
    remaining: &'a [String],
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct CommandDoc {
    name: String,
    description: String,
    usage: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    subcommands: Vec<CommandDoc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Envelope;
    use serde_json::json;

    fn sample_cli() -> AgentCli {
        AgentCli::new("wokhei", "Agent-first Nostr list CLI")
            .version("0.1.0")
            .command(
                Command::new("status", "Show relay and key status")
                    .usage("wokhei status")
                    .handler(|_req, _ctx| {
                        Ok(CommandOutput::new(json!({
                            "healthy": true,
                            "keys_configured": true
                        }))
                        .next_action(NextAction::new("wokhei status", "Re-check current status")))
                    }),
            )
            .command(
                Command::new("gateway", "Gateway operations").subcommand(
                    Command::new("stream", "Stream gateway events")
                        .usage("wokhei gateway stream [--follow]")
                        .handler(|_req, _ctx| {
                            Ok(CommandOutput::new(json!({
                                "stream": "ready"
                            })))
                        }),
                ),
            )
    }

    #[test]
    fn parse_invocation_handles_flags_and_positionals() {
        let invocation = parse_invocation([
            "wokhei",
            "create-header",
            "--relay",
            "ws://localhost:7777",
            "-d",
            "payload.json",
            "extra",
        ])
        .expect("invocation should parse");

        assert_eq!(invocation.program(), "wokhei");
        assert_eq!(invocation.flag("relay"), Some("ws://localhost:7777"));
        assert_eq!(invocation.flag("d"), Some("payload.json"));
        assert_eq!(
            invocation.positionals(),
            &[String::from("create-header"), String::from("extra")]
        );
    }

    #[test]
    fn root_returns_self_documenting_json_tree() {
        let cli = sample_cli();
        let run = cli.run_argv(["wokhei"]);

        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.result["commands"].is_array());
        assert!(!envelope.next_actions.is_empty());
        assert_eq!(run.exit_code(), 0);
    }

    #[test]
    fn command_success_returns_envelope() {
        let cli = sample_cli();
        let run = cli.run_argv(["wokhei", "status"]);

        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["healthy"], Value::Bool(true));
        assert!(!envelope.next_actions.is_empty());
    }

    #[test]
    fn unknown_command_returns_error_with_fix() {
        let cli = sample_cli();
        let run = cli.run_argv(["wokhei", "bogus"]);

        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, "UNKNOWN_COMMAND");
        assert!(!envelope.fix.is_empty());
        assert!(!envelope.next_actions.is_empty());
        assert_eq!(run.exit_code(), 1);
    }

    #[test]
    fn help_returns_json_not_plain_text() {
        let cli = sample_cli();
        let run = cli.run_argv(["wokhei", "gateway", "--help"]);

        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(
            envelope.result["name"],
            Value::String("gateway".to_string())
        );
        assert!(envelope.result["subcommands"].is_array());
    }

    #[test]
    fn command_error_uses_handler_code_and_fix() {
        let cli = AgentCli::new("wokhei", "Agent-first Nostr list CLI").command(
            Command::new("publish", "Publish JSON event").handler(|_req, _ctx| {
                Err(CommandError::new(
                    "invalid event json",
                    "INVALID_EVENT",
                    "Pass valid event JSON and required tags.",
                ))
            }),
        );

        let run = cli.run_argv(["wokhei", "publish"]);
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, "INVALID_EVENT");
        assert_eq!(envelope.fix, "Pass valid event JSON and required tags.");
    }

    // --- New parser edge-case tests ---

    #[test]
    fn negative_number_treated_as_positional() {
        let invocation =
            parse_invocation(["app", "status", "-123"]).expect("invocation should parse");
        assert_eq!(
            invocation.positionals(),
            &[String::from("status"), String::from("-123")]
        );
        assert!(invocation.flags().is_empty());
    }

    #[test]
    fn negative_float_treated_as_positional() {
        let invocation = parse_invocation(["app", "-3.14"]).expect("invocation should parse");
        assert_eq!(invocation.positionals(), &[String::from("-3.14")]);
    }

    #[test]
    fn long_flag_with_negative_value() {
        let invocation =
            parse_invocation(["app", "--count", "-1"]).expect("invocation should parse");
        assert_eq!(invocation.flag("count"), Some("-1"));
        assert!(invocation.positionals().is_empty());
    }

    #[test]
    fn short_flag_with_negative_value() {
        let invocation = parse_invocation(["app", "-n", "-5"]).expect("invocation should parse");
        assert_eq!(invocation.flag("n"), Some("-5"));
        assert!(invocation.positionals().is_empty());
    }

    #[test]
    fn long_flag_equals_syntax() {
        let invocation = parse_invocation(["app", "--count=-1"]).expect("invocation should parse");
        assert_eq!(invocation.flag("count"), Some("-1"));
    }

    #[test]
    fn double_dash_forces_positionals() {
        let invocation = parse_invocation(["app", "--", "--not-a-flag", "-abc"])
            .expect("invocation should parse");
        assert_eq!(
            invocation.positionals(),
            &[String::from("--not-a-flag"), String::from("-abc")]
        );
        assert!(invocation.flags().is_empty());
    }

    #[test]
    fn bundled_short_flags() {
        let invocation = parse_invocation(["app", "-abc"]).expect("invocation should parse");
        assert_eq!(invocation.flag("a"), Some("true"));
        assert_eq!(invocation.flag("b"), Some("true"));
        assert_eq!(invocation.flag("c"), Some("true"));
    }

    #[test]
    fn prompt_returns_cow_borrowed_for_flag() {
        let invocation =
            parse_invocation(["app", "--prompt", "hello world"]).expect("should parse");
        let request = CommandRequest {
            invocation: &invocation,
            command_path: &[],
            positionals: &[],
        };
        let prompt = request.prompt().expect("should have prompt");
        assert!(matches!(prompt, Cow::Borrowed(_)));
        assert_eq!(&*prompt, "hello world");
    }

    #[test]
    fn prompt_returns_cow_owned_for_positionals() {
        let invocation = parse_invocation(["app"]).expect("should parse");
        let positionals = vec!["foo".to_string(), "bar".to_string()];
        let request = CommandRequest {
            invocation: &invocation,
            command_path: &[],
            positionals: &positionals,
        };
        let prompt = request.prompt().expect("should have prompt");
        assert!(matches!(prompt, Cow::Owned(_)));
        assert_eq!(&*prompt, "foo bar");
    }
}
