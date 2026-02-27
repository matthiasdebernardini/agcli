use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::envelope::{ActionParam, Envelope, ErrorEnvelope, NextAction, SuccessEnvelope};

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

impl std::error::Error for ParseInvocationError {}

/// Returns true if `s` looks like a negative number (integer, float, or scientific notation).
fn looks_like_negative_number(s: &str) -> bool {
    s.starts_with('-') && s.len() >= 2 && s.parse::<f64>().is_ok()
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

    for token in &tokens {
        if positional_only {
            positionals.push(token.clone());
            continue;
        }

        match token.as_str() {
            "--" => {
                positional_only = true;
                continue;
            }
            "--help" | "-h" => {
                help_requested = true;
                continue;
            }
            _ => {}
        }

        // Long flags: --key=value or bare --key (bool)
        if let Some(flag) = token.strip_prefix("--") {
            if flag.is_empty() {
                return Err(ParseInvocationError::InvalidFlag(token.clone()));
            }

            if let Some((key, value)) = flag.split_once('=') {
                if key.is_empty() {
                    return Err(ParseInvocationError::InvalidFlag(token.clone()));
                }
                flags.insert(key.to_string(), value.to_string());
                continue;
            }

            // Bare --flag is always boolean
            flags.insert(flag.to_string(), "true".to_string());
            continue;
        }

        // Negative numbers as positionals: -123, -3.14
        if looks_like_negative_number(token) {
            positionals.push(token.clone());
            continue;
        }

        // Short flags: -x, -x=value, or -abc (bundled, all bool)
        if token.starts_with('-') && token.len() > 1 {
            let short = &token[1..];

            // Check for -k=value
            if let Some((key, value)) = short.split_once('=')
                && key.chars().count() == 1
            {
                flags.insert(key.to_string(), value.to_string());
                continue;
            }

            if short.chars().count() == 1 {
                // Single short flag: -x (always bool)
                flags.insert(short.to_string(), "true".to_string());
                continue;
            }

            // Bundled short flags: -abc (all bool)
            for ch in short.chars() {
                flags.insert(ch.to_string(), "true".to_string());
            }
            continue;
        }

        positionals.push(token.clone());
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
    pub retryable: bool,
    pub next_actions: Vec<NextAction>,
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for CommandError {}

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
            retryable: false,
            next_actions: Vec::new(),
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
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

/// Build a `NextAction` from a usage string and description.
///
/// Parses `<name>` placeholders (required positional) and `[--flag=<name>]`
/// or `[--flag <name>]` placeholders (optional flag) from the usage string
/// and auto-populates `params`. If no placeholders are found, the action is
/// literal (no `params`).
fn next_action_from_usage(usage: &str, description: impl Into<String>) -> NextAction {
    let mut action = NextAction::new(usage, description);
    let bytes = usage.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Optional flag placeholder: [--flag=<name>] or [--flag <name>]
        if bytes[i] == b'[' && i + 2 < len && bytes[i + 1] == b'-' && bytes[i + 2] == b'-' {
            // Find the closing bracket
            if let Some(close) = usage[i..].find(']') {
                let bracket_content = &usage[i + 1..i + close]; // strip [ and ]
                // Look for <name> inside
                if let Some(angle_start) = bracket_content.find('<')
                    && let Some(angle_end) = bracket_content[angle_start..].find('>')
                {
                    let param_name = &bracket_content[angle_start + 1..angle_start + angle_end];
                    if !param_name.is_empty() {
                        action = action.with_param(param_name, ActionParam::new().required(false));
                    }
                }
                i += close + 1;
                continue;
            }
        }

        // Positional placeholder: <name> (not inside [...])
        if bytes[i] == b'<'
            && let Some(close) = usage[i..].find('>')
        {
            let param_name = &usage[i + 1..i + close];
            if !param_name.is_empty() && !param_name.contains('.') {
                action = action.with_param(param_name, ActionParam::new().required(true));
            }
            i += close + 1;
            continue;
        }

        i += 1;
    }

    action
}

type CommandHandler = dyn for<'a> Fn(
        &'a CommandRequest<'a>,
        &'a mut ExecutionContext,
    )
        -> Pin<Box<dyn Future<Output = Result<CommandOutput, CommandError>> + Send + 'a>>
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

    /// Attach an async handler. Use with: `.handler(|req, ctx| Box::pin(async move { ... }))`
    pub fn handler<F>(mut self, f: F) -> Self
    where
        F: for<'a> Fn(
                &'a CommandRequest<'a>,
                &'a mut ExecutionContext,
            ) -> Pin<
                Box<dyn Future<Output = Result<CommandOutput, CommandError>> + Send + 'a>,
            > + Send
            + Sync
            + 'static,
    {
        self.handler = Some(Arc::new(f));
        self
    }

    /// Attach a synchronous handler (convenience wrapper).
    pub fn sync_handler<F>(self, f: F) -> Self
    where
        F: for<'a> Fn(
                &'a CommandRequest<'a>,
                &'a mut ExecutionContext,
            ) -> Result<CommandOutput, CommandError>
            + Send
            + Sync
            + 'static,
    {
        self.handler(move |req, ctx| {
            let result = f(req, ctx);
            Box::pin(async move { result })
        })
    }

    pub fn subcommand(mut self, command: Command) -> Self {
        debug_assert!(
            !self.subcommands.contains_key(&command.name),
            "duplicate subcommand: {}",
            command.name
        );
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
            format!("{program} {joined} [--flag=<value>] [args...]")
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
    schema_version: Option<String>,
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
            schema_version: None,
            commands: BTreeMap::new(),
            root_extra: Map::new(),
        }
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn schema_version(mut self, version: impl Into<String>) -> Self {
        self.schema_version = Some(version.into());
        self
    }

    pub fn command(mut self, command: Command) -> Self {
        debug_assert!(
            !self.commands.contains_key(&command.name),
            "duplicate command: {}",
            command.name
        );
        self.commands.insert(command.name.clone(), command);
        self
    }

    pub fn root_field(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.root_extra.insert(key.into(), value.into());
        self
    }

    pub async fn run_env(&self) -> Execution {
        let mut context = ExecutionContext::default();
        self.run_argv_with_context(std::env::args(), &mut context)
            .await
    }

    pub async fn run_env_with_context(&self, context: &mut ExecutionContext) -> Execution {
        self.run_argv_with_context(std::env::args(), context).await
    }

    pub async fn run_argv<I, S>(&self, args: I) -> Execution
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut context = ExecutionContext::default();
        self.run_argv_with_context(args, &mut context).await
    }

    pub async fn run_argv_with_context<I, S>(
        &self,
        args: I,
        context: &mut ExecutionContext,
    ) -> Execution
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
                    false,
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
                false,
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
                    false,
                    self.root_actions(),
                );
            }
        };

        if command.handler.is_none() && !command.subcommands.is_empty() {
            if resolved.remaining.is_empty() {
                return self.command_tree_execution(&invocation, &resolved.path, command);
            }
            let path_strs = path_refs(&resolved.path);
            return self.error_execution(
                invocation.command_line().to_string(),
                format!("unknown subcommand: {}", resolved.remaining[0]),
                "UNKNOWN_SUBCOMMAND",
                format!(
                    "Use one of the listed subcommands under `{}`.",
                    resolved.path.join(" ")
                ),
                false,
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
                    false,
                    self.root_actions(),
                );
            }
        };

        let request = CommandRequest {
            invocation: &invocation,
            command_path: &resolved.path,
            positionals: resolved.remaining,
        };

        let path_strs = path_refs(&resolved.path);
        match handler(&request, context).await {
            Ok(output) => {
                let next_actions =
                    self.ensure_next_actions(output.next_actions, &path_strs, command);
                Execution {
                    envelope: self
                        .success_envelope(
                            invocation.into_command_line(),
                            output.result,
                            next_actions,
                        )
                        .into(),
                }
            }
            Err(error) => {
                let next_actions =
                    self.ensure_next_actions(error.next_actions, &path_strs, command);
                self.error_execution(
                    invocation.into_command_line(),
                    error.message,
                    error.code,
                    error.fix,
                    error.retryable,
                    next_actions,
                )
            }
        }
    }

    fn success_envelope(
        &self,
        command: impl Into<String>,
        result: Value,
        next_actions: Vec<NextAction>,
    ) -> SuccessEnvelope {
        let mut envelope = SuccessEnvelope::new(command, result, next_actions);
        if let Some(ref sv) = self.schema_version {
            envelope = envelope.schema_version(sv.clone());
        }
        envelope
    }

    fn root_execution(&self, invocation: &Invocation) -> Execution {
        let result = self.root_result(invocation.program());
        Execution {
            envelope: self
                .success_envelope(
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
                false,
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
                    false,
                    self.root_actions(),
                );
            }
        };

        // Reject trailing unknown tokens when the command has subcommands
        if !resolved.remaining.is_empty() && !command.subcommands.is_empty() {
            let path_strs = path_refs(&resolved.path);
            return self.error_execution(
                invocation.command_line().to_string(),
                format!("unknown subcommand: {}", resolved.remaining[0]),
                "UNKNOWN_SUBCOMMAND",
                format!(
                    "Use one of the listed subcommands under `{}`.",
                    resolved.path.join(" ")
                ),
                false,
                self.subcommand_actions(&path_strs, command),
            );
        }

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
            envelope: self
                .success_envelope(
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

        // Insert user-provided extras first so core keys always win
        result.extend(self.root_extra.clone());

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

        Value::Object(result)
    }

    fn root_actions(&self) -> Vec<NextAction> {
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
                next_action_from_usage(&usage, command.description.clone())
            })
            .collect()
    }

    fn default_command_actions(&self, path: &[&str], command: &Command) -> Vec<NextAction> {
        if !command.default_next_actions.is_empty() {
            return command.default_next_actions.clone();
        }

        let usage = command.usage_or_default(&self.name, path);
        vec![
            next_action_from_usage(&usage, "Run this command template"),
            NextAction::new(self.name.clone(), "Inspect the full command tree"),
        ]
    }

    fn ensure_next_actions(
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

    fn subcommand_actions(&self, path: &[&str], command: &Command) -> Vec<NextAction> {
        if command.subcommands.is_empty() {
            return self.default_command_actions(path, command);
        }

        let mut path_buf: Vec<&str> = path.to_vec();
        let mut actions = Vec::with_capacity(command.subcommands.len() + 1);
        for sub in command.subcommands.values() {
            path_buf.push(&sub.name);
            let usage = sub.usage_or_default(&self.name, &path_buf);
            actions.push(next_action_from_usage(&usage, sub.description.clone()));
            path_buf.pop();
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
        retryable: bool,
        next_actions: Vec<NextAction>,
    ) -> Execution {
        let mut envelope =
            ErrorEnvelope::new(command, message, code, fix, next_actions).retryable(retryable);
        if let Some(ref sv) = self.schema_version {
            envelope = envelope.schema_version(sv.clone());
        }
        Execution {
            envelope: envelope.into(),
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
        self.command_docs_into(program, path_buf, commands, &mut docs, depth);
        docs
    }

    fn resolve_command<'a>(&'a self, positionals: &'a [String]) -> ResolvedCommand<'a> {
        let mut commands = &self.commands;
        let mut path = Vec::new();
        let mut current: Option<&Command> = None;

        for (idx, token) in positionals.iter().enumerate() {
            let Some(found) = commands.get(token) else {
                return ResolvedCommand {
                    command: current,
                    path,
                    remaining: &positionals[idx..],
                };
            };
            path.push(token.clone());
            current = Some(found);
            commands = &found.subcommands;
        }

        ResolvedCommand {
            command: current,
            path,
            remaining: &positionals[positionals.len()..],
        }
    }
}

fn path_refs(path: &[String]) -> Vec<&str> {
    path.iter().map(String::as_str).collect()
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
                    .sync_handler(|_req, _ctx| {
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
                        .sync_handler(|_req, _ctx| {
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
            "--relay=ws://localhost:7777",
            "-d=payload.json",
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

    #[tokio::test]
    async fn root_returns_self_documenting_json_tree() {
        let cli = sample_cli();
        let run = cli.run_argv(["wokhei"]).await;

        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.result["commands"].is_array());
        assert!(!envelope.next_actions.is_empty());
        assert_eq!(run.exit_code(), 0);
    }

    #[tokio::test]
    async fn command_success_returns_envelope() {
        let cli = sample_cli();
        let run = cli.run_argv(["wokhei", "status"]).await;

        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["healthy"], Value::Bool(true));
        assert!(!envelope.next_actions.is_empty());
    }

    #[tokio::test]
    async fn unknown_command_returns_error_with_fix() {
        let cli = sample_cli();
        let run = cli.run_argv(["wokhei", "bogus"]).await;

        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, "UNKNOWN_COMMAND");
        assert!(!envelope.fix.is_empty());
        assert!(!envelope.next_actions.is_empty());
        assert_eq!(run.exit_code(), 1);
    }

    #[tokio::test]
    async fn help_returns_json_not_plain_text() {
        let cli = sample_cli();
        let run = cli.run_argv(["wokhei", "gateway", "--help"]).await;

        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(
            envelope.result["name"],
            Value::String("gateway".to_string())
        );
        assert!(envelope.result["subcommands"].is_array());
    }

    #[tokio::test]
    async fn command_error_uses_handler_code_and_fix() {
        let cli = AgentCli::new("wokhei", "Agent-first Nostr list CLI").command(
            Command::new("publish", "Publish JSON event").sync_handler(|_req, _ctx| {
                Err(CommandError::new(
                    "invalid event json",
                    "INVALID_EVENT",
                    "Pass valid event JSON and required tags.",
                ))
            }),
        );

        let run = cli.run_argv(["wokhei", "publish"]).await;
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
        let invocation = parse_invocation(["app", "--count=-1"]).expect("invocation should parse");
        assert_eq!(invocation.flag("count"), Some("-1"));
        assert!(invocation.positionals().is_empty());
    }

    #[test]
    fn short_flag_with_negative_value() {
        let invocation = parse_invocation(["app", "-n=-5"]).expect("invocation should parse");
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
        let invocation = parse_invocation(["app", "--prompt=hello world"]).expect("should parse");
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

    // --- Strict flag parser tests ---

    #[test]
    fn bare_long_flag_is_boolean() {
        let invocation =
            parse_invocation(["app", "--verbose", "positional"]).expect("should parse");
        assert_eq!(invocation.flag("verbose"), Some("true"));
        assert_eq!(invocation.positionals(), &[String::from("positional")]);
    }

    #[test]
    fn bare_short_flag_is_boolean() {
        let invocation = parse_invocation(["app", "-v", "positional"]).expect("should parse");
        assert_eq!(invocation.flag("v"), Some("true"));
        assert_eq!(invocation.positionals(), &[String::from("positional")]);
    }

    #[test]
    fn long_flag_equals_carries_value() {
        let invocation = parse_invocation(["app", "--output=file.txt"]).expect("should parse");
        assert_eq!(invocation.flag("output"), Some("file.txt"));
        assert!(invocation.positionals().is_empty());
    }

    #[test]
    fn short_flag_equals_carries_value() {
        let invocation = parse_invocation(["app", "-o=file.txt"]).expect("should parse");
        assert_eq!(invocation.flag("o"), Some("file.txt"));
        assert!(invocation.positionals().is_empty());
    }

    #[test]
    fn strict_parser_does_not_swallow_commands() {
        // `--follow status` should NOT consume `status` as the flag value
        let invocation = parse_invocation(["app", "--follow", "status"]).expect("should parse");
        assert_eq!(invocation.flag("follow"), Some("true"));
        assert_eq!(invocation.positionals(), &[String::from("status")]);
    }

    #[test]
    fn scientific_notation_negative_number() {
        let invocation = parse_invocation(["app", "-1e3"]).expect("should parse");
        assert_eq!(invocation.positionals(), &[String::from("-1e3")]);
        assert!(invocation.flags().is_empty());
    }

    #[test]
    fn shorthand_decimal_negative_number() {
        let invocation = parse_invocation(["app", "-.5"]).expect("should parse");
        assert_eq!(invocation.positionals(), &[String::from("-.5")]);
        assert!(invocation.flags().is_empty());
    }

    // --- Help with trailing unknown tokens ---

    #[tokio::test]
    async fn help_with_trailing_unknown_returns_error() {
        let cli = sample_cli();
        let run = cli.run_argv(["wokhei", "gateway", "bogus", "--help"]).await;

        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, "UNKNOWN_SUBCOMMAND");
    }

    // --- root_field cannot shadow core keys ---

    #[tokio::test]
    async fn root_field_cannot_shadow_core_keys() {
        let cli = AgentCli::new("test", "Test CLI")
            .version("1.0.0")
            .root_field("description", json!("hacked"))
            .root_field("version", json!("hacked"))
            .root_field("commands", json!("hacked"));

        let run = cli.run_argv(["test"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        // Core keys must not be overwritten by root_field
        assert_eq!(envelope.result["description"], json!("Test CLI"));
        assert_eq!(envelope.result["version"], json!("1.0.0"));
        assert!(envelope.result["commands"].is_array());
    }

    // --- Duplicate command debug_assert ---

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "duplicate command")]
    fn duplicate_command_panics_in_debug() {
        AgentCli::new("test", "Test CLI")
            .command(Command::new("status", "first"))
            .command(Command::new("status", "second"));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "duplicate subcommand")]
    fn duplicate_subcommand_panics_in_debug() {
        Command::new("parent", "Parent")
            .subcommand(Command::new("child", "first"))
            .subcommand(Command::new("child", "second"));
    }

    // --- next_action_from_usage tests ---

    #[test]
    fn next_action_from_usage_literal_no_params() {
        let action = next_action_from_usage("wokhei status", "Check status");
        assert!(action.params.is_none());
        assert_eq!(action.command, "wokhei status");
    }

    #[test]
    fn next_action_from_usage_positional_placeholder() {
        let action = next_action_from_usage("wokhei run <run-id>", "Run a job");
        let params = action.params.as_ref().expect("params should be present");
        assert_eq!(params.len(), 1);
        let p = params.get("run-id").expect("run-id param");
        assert_eq!(p.required, Some(true));
    }

    #[test]
    fn next_action_from_usage_optional_flag_placeholder() {
        let action = next_action_from_usage("wokhei logs [--lines=<lines>]", "View logs");
        let params = action.params.as_ref().expect("params should be present");
        assert_eq!(params.len(), 1);
        let p = params.get("lines").expect("lines param");
        assert_eq!(p.required, Some(false));
    }

    #[test]
    fn next_action_from_usage_mixed_placeholders() {
        let action = next_action_from_usage(
            "wokhei send <event> [--data=<data>] [--follow]",
            "Send event",
        );
        let params = action.params.as_ref().expect("params should be present");
        assert_eq!(params.len(), 2);
        assert_eq!(params.get("event").unwrap().required, Some(true));
        assert_eq!(params.get("data").unwrap().required, Some(false));
        // [--follow] has no <placeholder> so no param entry
        assert!(params.get("follow").is_none());
    }

    #[test]
    fn next_action_from_usage_default_generated_usage() {
        // The default usage_or_default for commands with no custom usage
        let action = next_action_from_usage("prog cmd [--flag=<value>] [args...]", "Do something");
        let params = action.params.as_ref().expect("params should be present");
        // <value> from [--flag=<value>] should be parsed
        assert!(params.contains_key("value"));
        // [args...] has no angle brackets, no param
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
