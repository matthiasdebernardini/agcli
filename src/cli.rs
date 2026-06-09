use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::audit::{AuditReport, AuditSeverity};
use crate::doctor::Check;
use crate::envelope::{
    ActionParam, Envelope, ErrorEnvelope, ExitCode, NextAction, SuccessEnvelope,
};
use crate::project;

/// Maximum recursion depth for command tree traversal.
const MAX_COMMAND_DEPTH: usize = 32;

/// Flags the framework reserves and treats as pure booleans on every command
/// (when `reserved_flags` is enabled), so an agent can pass them anywhere
/// without the parser mistaking the next token for their value. `--select`
/// is intentionally absent: it is a *value* flag and uses the normal
/// consume-next-token rule.
const RESERVED_BOOL_FLAGS: &[&str] = &[
    "json", "dry-run", "compact", "stdin", "quiet", "yes", "no-input", "no-cache", "no-color",
];

fn is_reserved_bool(flag: &str) -> bool {
    RESERVED_BOOL_FLAGS.contains(&flag)
}

/// The framework-reserved agent-native flags with their semantics, in the
/// order they appear in the self-documenting root tree. `--select` is a value
/// flag; the rest are booleans (see [`RESERVED_BOOL_FLAGS`]).
const RESERVED_FLAG_DOCS: &[(&str, &str)] = &[
    (
        "--select=<a,b,c>",
        "Project the result to only these fields (top-level keys or `a.b` dot paths; maps over arrays).",
    ),
    (
        "--compact",
        "Drop null and empty result fields (or keep a command's declared high-gravity allowlist).",
    ),
    ("--quiet", "Omit `next_actions` from the envelope."),
    (
        "--dry-run",
        "Preview without mutating; read via `req.dry_run()`.",
    ),
    (
        "--yes / --no-input",
        "Assume yes; never prompt interactively (`req.assume_yes()`).",
    ),
    ("--no-cache", "Bypass any local cache (`req.no_cache()`)."),
    (
        "--no-color",
        "Emit machine-friendly, uncolored output (`req.no_color()`).",
    ),
    (
        "--stdin",
        "Read piped input; pair with `agcli::read_stdin()` (`req.wants_stdin()`).",
    ),
];

/// Every framework-reserved flag name (without the leading `--`), for runtime
/// discovery. These names are reserved whenever
/// [`AgentCli::reserved_flags`] is enabled (the default): the framework parses
/// and acts on them on every command. `select` is a value flag; the others are
/// parsed as booleans anywhere on the line.
pub fn reserved_flag_names() -> &'static [&'static str] {
    &[
        "select", "json", "dry-run", "compact", "stdin", "quiet", "yes", "no-input", "no-cache",
        "no-color",
    ]
}

/// Split a `--select` value (`"id,name, body"`) into trimmed, non-empty
/// field names.
fn parse_select_fields(raw: &str) -> Vec<&str> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Apply a reserved `--select` value to a handler result, guarding against the
/// silent-erasure footgun. A bare `--select` (parsed as the sentinel `"true"`),
/// an empty value, or a typo'd/array-only field name all project a non-empty
/// object down to `{}`. Rather than discard the handler's real output and
/// report a misleading `ok: true` with `result: {}`, this returns the original
/// result annotated with a `select_warning` that names the available fields, so
/// the agent keeps its data and learns how to correct the select. (An error
/// envelope is deliberately avoided: the handler already ran — possibly with
/// side effects — so signalling failure could trigger an unwanted retry.)
fn apply_select_flag(result: Value, raw: &str) -> Value {
    let fields = parse_select_fields(raw);
    if fields.is_empty() {
        return annotate_select_warning(
            result,
            "--select was given no field names. Returning the full result. \
             Re-run with --select=<field>[,<field>...]."
                .to_string(),
        );
    }
    let projected = project::select(&result, &fields);
    let collapsed = matches!(&result, Value::Object(map) if !map.is_empty())
        && matches!(&projected, Value::Object(map) if map.is_empty());
    if collapsed {
        let available = match &result {
            Value::Object(map) => map.keys().cloned().collect::<Vec<_>>().join(", "),
            _ => String::new(),
        };
        return annotate_select_warning(
            result,
            format!(
                "--select={} matched no fields. Available top-level fields: {available}. \
                 Returning the full result; re-run --select with a valid field name.",
                fields.join(",")
            ),
        );
    }
    projected
}

/// Attach a `select_warning` note to an object result. No-op for non-objects
/// (arrays/scalars are returned unchanged so the element-wise select semantics
/// are preserved).
fn annotate_select_warning(result: Value, warning: String) -> Value {
    match result {
        Value::Object(mut map) => {
            map.insert("select_warning".to_string(), Value::String(warning));
            Value::Object(map)
        }
        other => other,
    }
}

/// Best-effort human-readable message from a caught panic payload.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Read all of stdin to a string. Pairs with the `--stdin` convention so a
/// handler can accept piped input: `if req.wants_stdin() { read_stdin().await }`.
pub async fn read_stdin() -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut buf = String::new();
    tokio::io::stdin().read_to_string(&mut buf).await?;
    Ok(buf)
}

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

/// Parse argv into an `Invocation` without any boolean-flag schema.
///
/// A bare long flag (`--key`) followed by a non-flag token consumes that
/// token as its value (`--key value` ≡ `--key=value`). This matches the
/// space-separated form used in HATEOAS templates like `[--flag <value>]`.
///
/// If you have a known set of boolean flags (e.g. derived from a command's
/// usage string), prefer [`parse_invocation_with_bool_flags`] so that
/// `--bool-flag positional` does **not** silently consume the positional.
pub fn parse_invocation<I, S>(args: I) -> Result<Invocation, ParseInvocationError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    parse_invocation_with_bool_flags(args, |_| false)
}

/// Parse argv into an `Invocation`, treating any flag for which `is_bool`
/// returns `true` as a pure boolean (it never consumes the next token).
///
/// Used internally by [`AgentCli::run_argv_with_context`] to honor each
/// command's usage-string schema. Exposed publicly so callers parsing argv
/// outside the `AgentCli` runtime can opt in to the same disambiguation.
pub fn parse_invocation_with_bool_flags<I, S, F>(
    args: I,
    is_bool: F,
) -> Result<Invocation, ParseInvocationError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    F: Fn(&str) -> bool,
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
    // A bare `--key` whose value form we don't yet know. If the next token is
    // a plain value, it becomes the flag's value; otherwise the flag is bool.
    let mut pending_flag: Option<String> = None;

    let flush_pending = |pending: &mut Option<String>, flags: &mut HashMap<String, String>| {
        if let Some(key) = pending.take() {
            flags.insert(key, "true".to_string());
        }
    };

    for token in &tokens {
        if positional_only {
            flush_pending(&mut pending_flag, &mut flags);
            positionals.push(token.clone());
            continue;
        }

        match token.as_str() {
            "--" => {
                flush_pending(&mut pending_flag, &mut flags);
                positional_only = true;
                continue;
            }
            "--help" | "-h" => {
                flush_pending(&mut pending_flag, &mut flags);
                help_requested = true;
                continue;
            }
            _ => {}
        }

        // Long flags: --key=value or bare --key
        if let Some(flag) = token.strip_prefix("--") {
            if flag.is_empty() {
                return Err(ParseInvocationError::InvalidFlag(token.clone()));
            }

            // Starting a new flag flushes any prior pending bare flag.
            flush_pending(&mut pending_flag, &mut flags);

            if let Some((key, value)) = flag.split_once('=') {
                if key.is_empty() {
                    return Err(ParseInvocationError::InvalidFlag(token.clone()));
                }
                flags.insert(key.to_string(), value.to_string());
                continue;
            }

            // Bare --flag: known boolean, or candidate for next-token value.
            if is_bool(flag) {
                flags.insert(flag.to_string(), "true".to_string());
            } else {
                pending_flag = Some(flag.to_string());
            }
            continue;
        }

        // Negative numbers as positionals: -123, -3.14
        if looks_like_negative_number(token) {
            match pending_flag.take() {
                Some(key) => {
                    flags.insert(key, token.clone());
                }
                None => positionals.push(token.clone()),
            }
            continue;
        }

        // Short flags: -x, -x=value, or -abc (bundled, all bool)
        if token.starts_with('-') && token.len() > 1 {
            // Encountering another flag resolves the pending bare flag as bool.
            flush_pending(&mut pending_flag, &mut flags);

            let short = &token[1..];

            // Check for -k=value
            if let Some((key, value)) = short.split_once('=')
                && key.chars().count() == 1
            {
                flags.insert(key.to_string(), value.to_string());
                continue;
            }

            if short.chars().count() == 1 {
                if is_bool(short) {
                    flags.insert(short.to_string(), "true".to_string());
                } else {
                    pending_flag = Some(short.to_string());
                }
                continue;
            }

            // Bundled short flags: -abc (all bool)
            for ch in short.chars() {
                flags.insert(ch.to_string(), "true".to_string());
            }
            continue;
        }

        // Plain positional. If a flag is pending, this token is its value.
        match pending_flag.take() {
            Some(key) => {
                flags.insert(key, token.clone());
            }
            None => positionals.push(token.clone()),
        }
    }

    // End of args: any pending bare flag becomes boolean.
    flush_pending(&mut pending_flag, &mut flags);

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

    // --- Typed argument/flag accessors ---
    //
    // Fold the missing/parse boilerplate into a `CommandError` with
    // conventional codes (`MISSING_ARG`, `INVALID_ARG`, `INVALID_FLAG`) and a
    // generated `fix`, so every agcli CLI reports argument errors the same way
    // instead of each author hand-rolling slightly different ones.

    /// Return positional `index`, or a `MISSING_ARG` error naming it.
    pub fn require_arg(&self, index: usize, name: &str) -> Result<&str, CommandError> {
        self.arg(index).ok_or_else(|| {
            CommandError::new(
                format!("missing argument <{name}>"),
                "MISSING_ARG",
                format!("Provide <{name}> as positional argument {index}."),
            )
        })
    }

    /// Parse positional `index` into `T`, erroring with `MISSING_ARG` when
    /// absent or `INVALID_ARG` when it does not parse.
    pub fn arg_parse<T>(&self, index: usize, name: &str) -> Result<T, CommandError>
    where
        T: std::str::FromStr,
    {
        let raw = self.require_arg(index, name)?;
        raw.parse::<T>().map_err(|_| {
            CommandError::new(
                format!("argument <{name}> is not valid: {raw:?}"),
                "INVALID_ARG",
                format!("Pass a valid value for <{name}>."),
            )
        })
    }

    /// Parse flag `key` into `T` when present. `Ok(None)` when the flag is
    /// absent; `INVALID_FLAG` when present but unparseable.
    pub fn flag_parse<T>(&self, key: &str) -> Result<Option<T>, CommandError>
    where
        T: std::str::FromStr,
    {
        match self.flag(key) {
            None => Ok(None),
            Some(raw) => raw.parse::<T>().map(Some).map_err(|_| {
                CommandError::new(
                    format!("flag --{key} is not valid: {raw:?}"),
                    "INVALID_FLAG",
                    format!("Pass a valid value for --{key}."),
                )
            }),
        }
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

    // --- Standard agent-native flag accessors ---
    //
    // These read the conventional flag vocabulary so every agcli CLI speaks
    // the same dialect. The framework already parses the reserved booleans
    // anywhere on the line (see `RESERVED_BOOL_FLAGS`); these just give the
    // handler a typed read.

    /// `--dry-run`: preview without mutating.
    pub fn dry_run(&self) -> bool {
        self.flag("dry-run").is_some()
    }

    /// `--quiet`: suppress non-essential output.
    pub fn quiet(&self) -> bool {
        self.flag("quiet").is_some()
    }

    /// `--yes` / `--no-input`: assume yes; never prompt interactively.
    pub fn assume_yes(&self) -> bool {
        self.flag("yes").is_some() || self.flag("no-input").is_some()
    }

    /// `--no-cache`: bypass any local cache.
    pub fn no_cache(&self) -> bool {
        self.flag("no-cache").is_some()
    }

    /// `--no-color`: emit machine-friendly, uncolored output.
    pub fn no_color(&self) -> bool {
        self.flag("no-color").is_some()
    }

    /// `--compact`: caller asked for high-gravity fields only.
    pub fn compact(&self) -> bool {
        self.flag("compact").is_some()
    }

    /// `--stdin`: caller intends to pipe input. Pair with [`read_stdin`].
    pub fn wants_stdin(&self) -> bool {
        self.flag("stdin").is_some()
    }

    /// `--select=a,b,c`: the requested field projection, split and trimmed.
    /// `None` when the flag is absent; empty entries are dropped.
    pub fn select(&self) -> Option<Vec<&str>> {
        self.flag("select").map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect()
        })
    }
}

/// Success payload returned from a command handler.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandOutput {
    result: Value,
    next_actions: Vec<NextAction>,
    /// Optional process exit-code override. `None` → [`ExitCode::SUCCESS`].
    /// Lets a command report a non-zero status while still emitting an
    /// `ok: true` envelope (e.g. `doctor` reporting an unhealthy system).
    exit_code: Option<i32>,
    /// When `--compact` is requested, project to these fields instead of the
    /// generic null/empty drop. `None` → generic compaction.
    compact_fields: Option<Vec<String>>,
}

impl CommandOutput {
    pub fn new(result: Value) -> Self {
        Self {
            result,
            next_actions: Vec::new(),
            exit_code: None,
            compact_fields: None,
        }
    }

    pub fn from_serializable<T>(value: T) -> Result<Self, serde_json::Error>
    where
        T: Serialize,
    {
        Ok(Self::new(serde_json::to_value(value)?))
    }

    /// Build a bounded list result: `{ items, count, total, truncated }`.
    /// `count == total` and `truncated == false`.
    pub fn list(items: Vec<Value>) -> Self {
        let total = items.len();
        Self::list_truncated(items, total)
    }

    /// Build a bounded list result where `total` may exceed the returned
    /// items. When truncated, a `guidance` string nudges the agent to narrow
    /// the query (the framework also always offers the command template as a
    /// `next_action`).
    pub fn list_truncated(items: Vec<Value>, total: usize) -> Self {
        let count = items.len();
        let truncated = count < total;
        let mut result = serde_json::Map::new();
        result.insert("items".to_string(), Value::Array(items));
        result.insert("count".to_string(), json!(count));
        result.insert("total".to_string(), json!(total));
        result.insert("truncated".to_string(), json!(truncated));
        if truncated {
            result.insert(
                "guidance".to_string(),
                json!(format!(
                    "Showing {count} of {total} results. Narrow with --limit, --select, or filter flags."
                )),
            );
        }
        Self::new(Value::Object(result))
    }

    pub fn next_action(mut self, action: NextAction) -> Self {
        self.next_actions.push(action);
        self
    }

    pub fn next_actions(mut self, actions: Vec<NextAction>) -> Self {
        self.next_actions.extend(actions);
        self
    }

    /// Override the process exit code while still reporting success.
    pub fn exit_code(mut self, code: i32) -> Self {
        self.exit_code = Some(code);
        self
    }

    /// Declare the high-gravity fields `--compact` should keep for this
    /// output. Without this, `--compact` falls back to dropping null/empty
    /// fields generically.
    pub fn compact_fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.compact_fields = Some(fields.into_iter().map(Into::into).collect());
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
    /// Optional typed process exit code. `None` → [`ExitCode::ERROR`]. Set it
    /// with [`CommandError::exit_code`] using an [`ExitCode`] constant so an
    /// agent can branch on the failure class without parsing the message.
    pub exit_code: Option<i32>,
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
            exit_code: None,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Set the typed process exit code (use an [`ExitCode`] constant). An
    /// error envelope with `ExitCode::SUCCESS` (0) is contradictory — a shell
    /// or agent reads it as success — so it is rejected by a `debug_assert` in
    /// development builds.
    pub fn exit_code(mut self, code: i32) -> Self {
        debug_assert_ne!(
            code,
            ExitCode::SUCCESS,
            "an error must not carry a success (0) exit code"
        );
        self.exit_code = Some(code);
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
/// A bare `<name>` placeholder becomes a **required** positional param. Any
/// placeholder inside brackets is **optional**: `[--flag=<name>]`,
/// `[--flag <name>]`, short value flags `[-v <level>]`, and bare optional
/// positionals `[<optional>]`. Brackets without a `<...>` placeholder
/// (`[--follow]`, `[args...]`) contribute no param. If no placeholders are
/// found, the action is literal (no `params`).
fn next_action_from_usage(usage: &str, description: impl Into<String>) -> NextAction {
    let mut action = NextAction::new(usage, description);
    let bytes = usage.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Any bracketed token is optional: `[--flag=<name>]`, `[--flag <name>]`,
        // short value flags `[-v <level>]`, and bare optional positionals
        // `[<optional>]` all map their inner `<name>` placeholder to an optional
        // param. A bracket without a `<...>` placeholder (e.g. `[--follow]`,
        // `[args...]`) contributes no param.
        if bytes[i] == b'['
            && let Some(close) = usage[i..].find(']')
        {
            let bracket_content = &usage[i + 1..i + close]; // strip [ and ]
            if let Some(angle_start) = bracket_content.find('<')
                && let Some(angle_end) = bracket_content[angle_start..].find('>')
            {
                let param_name = &bracket_content[angle_start + 1..angle_start + angle_end];
                if !param_name.is_empty() && !param_name.contains('.') {
                    action = action.with_param(param_name, ActionParam::new().required(false));
                }
            }
            i += close + 1;
            continue;
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
    /// When false, the framework-reserved `--select`/`--compact` projection is
    /// not applied to this command's result. Used by the built-in `doctor`
    /// command so narrowing can never strip the actionable `fix` strings out of
    /// an unhealthy report.
    apply_reserved_projection: bool,
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
            apply_reserved_projection: true,
        }
    }

    pub fn usage(mut self, usage: impl Into<String>) -> Self {
        self.usage = Some(usage.into());
        self
    }

    /// Attach an async handler. The closure returns a boxed future:
    /// `.handler(|req, ctx| Box::pin(async move { ... }))`.
    ///
    /// Because the future captures by `move`, read any borrowed request data
    /// into owned locals *before* the `async move` block — including the typed
    /// helpers, which borrow `req`:
    ///
    /// ```ignore
    /// .handler(|req, _ctx| {
    ///     // Borrow first…
    ///     let source = req.arg(0).unwrap_or("worker").to_string();
    ///     let lines = req.flag_parse::<usize>("lines");
    ///     Box::pin(async move {
    ///         // …then move the owned values into the future.
    ///         let lines = lines?.unwrap_or(20);
    ///         Ok(CommandOutput::new(json!({ "source": source, "lines": lines })))
    ///     })
    /// })
    /// ```
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
    reserved_flags: bool,
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
            reserved_flags: true,
        }
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Toggle the framework-reserved agent-native flags (`--select`,
    /// `--compact`, `--quiet`, and the rest — see [`reserved_flag_names`] for
    /// the full set).
    ///
    /// Enabled by default: every command transparently supports them and the
    /// framework applies `--select`/`--compact`/`--quiet` to the result.
    /// Disable to take full manual control of flag parsing and output when a
    /// command needs one of these names with conflicting semantics.
    pub fn reserved_flags(mut self, enabled: bool) -> Self {
        self.reserved_flags = enabled;
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

    /// Register a built-in `doctor` command that runs `checks` and reports a
    /// structured health envelope `{ healthy, checks: [...] }`. When any check
    /// fails the command still returns an `ok: true` envelope (the report
    /// succeeded) but carries the failing check's exit code so a shell or
    /// agent sees a non-zero status. See [`Check`] and [`crate::CheckResult`].
    pub fn doctor(self, checks: Vec<Check>) -> Self {
        let usage = format!("{} doctor", self.name);
        let checks = Arc::new(checks);
        let mut command = Command::new("doctor", "Run environment health checks")
            .usage(usage)
            .handler(move |_req, _ctx| {
                let checks = Arc::clone(&checks);
                Box::pin(async move {
                    let mut entries = Vec::with_capacity(checks.len());
                    let mut healthy = true;
                    let mut fail_exit: Option<i32> = None;
                    for check in checks.iter() {
                        let result = check.run().await;
                        if !result.ok {
                            healthy = false;
                            // Prefer a specific (non-ERROR) exit code over the
                            // generic ERROR so the shell sees the most
                            // actionable failure class regardless of the order
                            // checks were registered in.
                            fail_exit = Some(match fail_exit {
                                None => check.exit_code,
                                Some(prev) if prev == ExitCode::ERROR => check.exit_code,
                                Some(prev) => prev,
                            });
                        }
                        let mut entry = Map::new();
                        entry.insert("name".to_string(), json!(check.name()));
                        entry.insert("ok".to_string(), json!(result.ok));
                        if let Some(detail) = result.detail {
                            entry.insert("detail".to_string(), json!(detail));
                        }
                        if let Some(fix) = result.fix {
                            entry.insert("fix".to_string(), json!(fix));
                        }
                        entries.push(Value::Object(entry));
                    }
                    let mut output = CommandOutput::new(json!({
                        "healthy": healthy,
                        "checks": entries,
                    }));
                    if !healthy {
                        output = output.exit_code(fail_exit.unwrap_or(ExitCode::ERROR));
                    }
                    Ok(output)
                })
            });
        // Never narrow the doctor report: `--select`/`--compact` must not be
        // able to strip the per-check `fix` strings from an unhealthy result.
        command.apply_reserved_projection = false;
        self.command(command)
    }

    /// Statically audit this CLI definition. Validates HATEOAS integrity
    /// (every declared `next_action` resolves to a real command), surfaces
    /// dead-end and undocumented commands, and returns a structured
    /// [`AuditReport`]. Intended for a downstream test:
    /// `assert!(cli.audit().is_clean())`.
    pub fn audit(&self) -> AuditReport {
        let mut report = AuditReport::default();
        let mut path_buf: Vec<&str> = Vec::new();
        self.audit_commands(&self.commands, &mut path_buf, &mut report, 0);
        report
    }

    fn audit_commands<'a>(
        &self,
        commands: &'a BTreeMap<String, Command>,
        path_buf: &mut Vec<&'a str>,
        report: &mut AuditReport,
        depth: usize,
    ) {
        if depth >= MAX_COMMAND_DEPTH {
            return;
        }
        for command in commands.values() {
            path_buf.push(&command.name);
            let path = path_buf.join(" ");

            if command.handler.is_none() && command.subcommands.is_empty() {
                report.push(
                    AuditSeverity::Error,
                    "DEAD_END_COMMAND",
                    &path,
                    "command has neither a handler nor subcommands; invoking it always errors",
                );
            }
            if command.description.trim().is_empty() {
                report.push(
                    AuditSeverity::Warning,
                    "EMPTY_DESCRIPTION",
                    &path,
                    "command has an empty description; agents rely on it to choose commands",
                );
            }
            if command.handler.is_some() && command.usage.is_none() {
                report.push(
                    AuditSeverity::Warning,
                    "MISSING_USAGE",
                    &path,
                    "handler command has no usage string; a framework default is used instead",
                );
            }
            for action in &command.default_next_actions {
                if !self.next_action_resolves(&action.command) {
                    report.push(
                        AuditSeverity::Error,
                        "DANGLING_NEXT_ACTION",
                        &path,
                        format!(
                            "default next_action `{}` does not resolve to a known command",
                            action.command
                        ),
                    );
                }
            }

            self.audit_commands(&command.subcommands, path_buf, report, depth + 1);
            path_buf.pop();
        }
    }

    /// True if the leading command tokens of a `next_action` template resolve
    /// to a real command path. Parsing stops at the first placeholder/flag
    /// token (`<arg>`, `[--flag]`, `-x`, `key=value`).
    ///
    /// A bare program-name reference (or an empty template) is the valid root
    /// affordance and resolves. But a template that — after dropping an optional
    /// leading program name — leads straight into a placeholder/flag matches
    /// *zero* command tokens and names no runnable command, so it is rejected
    /// (it would be a dead link for an agent following the trail).
    fn next_action_resolves(&self, template: &str) -> bool {
        let mut tokens = template.split_whitespace().peekable();
        if tokens.peek() == Some(&self.name.as_str()) {
            tokens.next();
        }
        let mut commands = &self.commands;
        let mut matched_command = false;
        let mut saw_token = false;
        for token in tokens {
            saw_token = true;
            if token.starts_with('<')
                || token.starts_with('[')
                || token.starts_with('-')
                || token.contains('=')
            {
                break;
            }
            match commands.get(token) {
                Some(found) => {
                    commands = &found.subcommands;
                    matched_command = true;
                }
                None => return false,
            }
        }
        // Resolved if it matched a real command, or it's the bare-root
        // affordance (no tokens after the optional program name). A template
        // that had tokens but matched no command (leading placeholder/flag) is
        // a dead link.
        matched_command || !saw_token
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

        let preliminary_path = self.preliminary_command_path(&argv);
        let bool_flags = Self::path_bool_flag_set(&preliminary_path);
        let reserved = self.reserved_flags;
        let invocation =
            match parse_invocation_with_bool_flags(argv.iter().map(String::as_str), |flag| {
                bool_flags.contains(flag) || (reserved && is_reserved_bool(flag))
            }) {
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
            let valid: Vec<&str> = self.commands.keys().map(String::as_str).collect();
            return self.error_execution(
                invocation.command_line().to_string(),
                format!("unknown command: {unknown}"),
                "UNKNOWN_COMMAND",
                unknown_command_fix("command", &unknown, &valid),
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
            let valid: Vec<&str> = command.subcommands.keys().map(String::as_str).collect();
            return self.error_execution(
                invocation.command_line().to_string(),
                format!("unknown subcommand: {}", resolved.remaining[0]),
                "UNKNOWN_SUBCOMMAND",
                unknown_command_fix("subcommand", &resolved.remaining[0], &valid),
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

        // Guard the user-supplied handler future against panics. Any
        // unwrap/expect/index/overflow in agent-written handler code would
        // otherwise unwind past the envelope machinery, printing nothing to
        // stdout and exiting 101 — the exact non-JSON failure the framework
        // exists to prevent. Catch the unwind and synthesize a structured
        // HANDLER_PANIC envelope so "JSON always" holds even for buggy
        // handlers. (The default panic hook may still print to stderr; stdout
        // — what an agent parses — stays valid JSON.)
        let mut handler_future = handler(&request, context);
        let handler_result = std::future::poll_fn(|cx| {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handler_future.as_mut().poll(cx)
            })) {
                Ok(poll) => poll.map(Ok),
                Err(payload) => std::task::Poll::Ready(Err(payload)),
            }
        })
        .await;
        // Drop the future to release its borrow of `request`/`invocation`
        // before the arms consume `invocation`.
        drop(handler_future);

        match handler_result {
            Ok(Ok(output)) => {
                let CommandOutput {
                    mut result,
                    next_actions: handler_actions,
                    exit_code: output_exit,
                    compact_fields,
                } = output;
                let mut next_actions =
                    self.ensure_next_actions(handler_actions, &path_strs, command);

                // Apply the framework-reserved output flags centrally so every
                // command supports --select / --compact / --quiet for free.
                if self.reserved_flags {
                    if command.apply_reserved_projection {
                        if let Some(raw) = invocation.flag("select") {
                            result = apply_select_flag(result, raw);
                        }
                        if invocation.flag("compact").is_some() {
                            result = match &compact_fields {
                                Some(fields) => {
                                    let refs: Vec<&str> =
                                        fields.iter().map(String::as_str).collect();
                                    project::select(&result, &refs)
                                }
                                None => project::compact(&result),
                            };
                        }
                    }
                    if invocation.flag("quiet").is_some() {
                        next_actions.clear();
                    }
                }

                let envelope = self
                    .success_envelope(invocation.into_command_line(), result, next_actions)
                    .exit_code(output_exit.unwrap_or(ExitCode::SUCCESS));
                Execution {
                    envelope: envelope.into(),
                }
            }
            Ok(Err(error)) => {
                let exit = error.exit_code.unwrap_or(ExitCode::ERROR);
                let next_actions =
                    self.ensure_next_actions(error.next_actions, &path_strs, command);
                Execution {
                    envelope: self
                        .build_error_envelope(
                            invocation.into_command_line(),
                            error.message,
                            error.code,
                            error.fix,
                            error.retryable,
                            exit,
                            next_actions,
                        )
                        .into(),
                }
            }
            Err(payload) => {
                let detail = panic_payload_message(payload.as_ref());
                let next_actions = self.default_command_actions(&path_strs, command);
                Execution {
                    envelope: self
                        .build_error_envelope(
                            invocation.into_command_line(),
                            format!("handler panicked: {detail}"),
                            "HANDLER_PANIC",
                            "This is a bug in the command handler, not the invocation. \
                             Inspect the root command tree and report the panic.",
                            false,
                            ExitCode::ERROR,
                            next_actions,
                        )
                        .into(),
                }
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
            let unknown = invocation.positionals()[0].clone();
            let valid: Vec<&str> = self.commands.keys().map(String::as_str).collect();
            return self.error_execution(
                invocation.command_line().to_string(),
                format!("unknown command: {unknown}"),
                "UNKNOWN_COMMAND",
                unknown_command_fix("command", &unknown, &valid),
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
            let valid: Vec<&str> = command.subcommands.keys().map(String::as_str).collect();
            return self.error_execution(
                invocation.command_line().to_string(),
                format!("unknown subcommand: {}", resolved.remaining[0]),
                "UNKNOWN_SUBCOMMAND",
                unknown_command_fix("subcommand", &resolved.remaining[0], &valid),
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

    /// Build an error `Execution` for a *framework-raised* error. Every such
    /// error (parse failure, unknown command/subcommand, missing handler) is
    /// a usage error, so it carries [`ExitCode::USAGE`]. Handler-raised errors
    /// do not go through here — they honor the handler's own exit code in the
    /// `run_argv_with_context` error branch.
    fn error_execution(
        &self,
        command: String,
        message: impl Into<String>,
        code: impl Into<String>,
        fix: impl Into<String>,
        retryable: bool,
        next_actions: Vec<NextAction>,
    ) -> Execution {
        Execution {
            envelope: self
                .build_error_envelope(
                    command,
                    message,
                    code,
                    fix,
                    retryable,
                    ExitCode::USAGE,
                    next_actions,
                )
                .into(),
        }
    }

    /// Construct an [`ErrorEnvelope`] with an explicit exit code, applying the
    /// CLI's `schema_version` if set.
    #[allow(clippy::too_many_arguments)]
    fn build_error_envelope(
        &self,
        command: String,
        message: impl Into<String>,
        code: impl Into<String>,
        fix: impl Into<String>,
        retryable: bool,
        exit_code: i32,
        next_actions: Vec<NextAction>,
    ) -> ErrorEnvelope {
        let mut envelope = ErrorEnvelope::new(command, message, code, fix, next_actions)
            .retryable(retryable)
            .exit_code(exit_code);
        if let Some(ref sv) = self.schema_version {
            envelope = envelope.schema_version(sv.clone());
        }
        envelope
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

    /// Collect every flag name declared as a pure boolean (`[--flag]`) across
    /// the entire command tree, including subcommands. Used only as a Pass 1
    /// heuristic by [`Self::preliminary_command_path`] so that bare bool flags
    /// in the input don't accidentally consume tokens that might be command
    /// names. The final flag/positional split honors the resolved command
    /// path's schema, not this global union — see [`Self::path_bool_flag_set`].
    fn global_bool_flag_set(&self) -> HashSet<String> {
        let mut set = HashSet::new();
        for command in self.commands.values() {
            collect_bool_flags(command, &mut set, 0);
        }
        set
    }

    /// Pass 1 of command resolution: walk argv with the global bool-flag
    /// union as a heuristic, then walk the resulting positionals against the
    /// command tree to find the deepest matching command path.
    ///
    /// The result feeds [`Self::path_bool_flag_set`] so the actual parse in
    /// `run_argv_with_context` only sees bool flags declared by commands on
    /// the resolved path — never flags declared by unrelated siblings.
    fn preliminary_command_path<'a>(&'a self, argv: &[String]) -> Vec<&'a Command> {
        let union_bools = self.global_bool_flag_set();
        let reserved = self.reserved_flags;
        let invocation =
            match parse_invocation_with_bool_flags(argv.iter().map(String::as_str), |flag| {
                union_bools.contains(flag) || (reserved && is_reserved_bool(flag))
            }) {
                Ok(value) => value,
                Err(_) => return Vec::new(),
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
                extract_bool_flag_names(usage, &mut set);
            }
        }
        set
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

/// Corrective `fix` text for an unknown command/subcommand error.
///
/// Lists every valid name inline so a *semantic* miss — e.g. guessing `list`
/// when the verb is `history`, which no edit-distance check would ever relate —
/// is corrected on the first read instead of triggering another blind guess.
/// When the bad token also looks like a typo of a real name, a nearest-match
/// nudge is prefixed. `next_actions` already carry the full templates; this puts
/// the names where the agent reads first.
fn unknown_command_fix(scope: &str, bad: &str, valid: &[&str]) -> String {
    if valid.is_empty() {
        return format!("Run the root command to inspect the available {scope}s.");
    }
    use std::fmt::Write as _;
    let mut fix = String::new();
    if let Some(near) = nearest_name(bad, valid) {
        let _ = write!(&mut fix, "Did you mean `{near}`? ");
    }
    let _ = write!(&mut fix, "Valid {scope}s: {}.", valid.join(", "));
    fix
}

/// Closest candidate by case-insensitive Levenshtein distance, gated so only a
/// plausible typo is offered: edit distance ≤ 2 and strictly less than the
/// candidate's length (so an unrelated short word can't match by coincidence).
fn nearest_name(bad: &str, valid: &[&str]) -> Option<String> {
    let bad_lower = bad.to_lowercase();
    valid
        .iter()
        .map(|cand| (*cand, levenshtein(&bad_lower, &cand.to_lowercase())))
        .filter(|(cand, dist)| *dist <= 2 && *dist < cand.chars().count().max(1))
        .min_by_key(|(_, dist)| *dist)
        .map(|(cand, _)| cand.to_string())
}

/// Classic single-row-DP Levenshtein edit distance. Inputs are command names
/// (a handful of chars), so the per-call allocation is negligible.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

fn collect_bool_flags(command: &Command, set: &mut HashSet<String>, depth: usize) {
    if depth >= MAX_COMMAND_DEPTH {
        return;
    }
    if let Some(usage) = &command.usage {
        extract_bool_flag_names(usage, set);
    }
    for sub in command.subcommands.values() {
        collect_bool_flags(sub, set, depth + 1);
    }
}

/// Pull boolean flag names out of a usage string. Recognizes bracketed
/// optional flags of the form `[--name]` (and short `[-x]`). Skips any
/// bracketed flag that carries a `<value>` placeholder or `=` — those are
/// value flags, not booleans.
fn extract_bool_flag_names(usage: &str, set: &mut HashSet<String>) {
    let bytes = usage.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let Some(close_rel) = usage[i + 1..].find(']') else {
            break;
        };
        let inner = &usage[i + 1..i + 1 + close_rel];
        i += close_rel + 2;
        if inner.contains('<') || inner.contains('=') {
            continue;
        }
        if let Some(rest) = inner.strip_prefix("--") {
            let name = rest.split_whitespace().next().unwrap_or("");
            if !name.is_empty() {
                set.insert(name.to_string());
            }
        } else if let Some(rest) = inner.strip_prefix('-') {
            // Short single-char or bundled boolean: `[-x]` or `[-abc]`.
            let token = rest.split_whitespace().next().unwrap_or("");
            for ch in token.chars() {
                set.insert(ch.to_string());
            }
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
                        Box::pin(async move {
                            Ok(CommandOutput::new(json!({
                                "healthy": true,
                                "keys_configured": true
                            }))
                            .next_action(NextAction::new(
                                "wokhei status",
                                "Re-check current status",
                            )))
                        })
                    }),
            )
            .command(
                Command::new("gateway", "Gateway operations").subcommand(
                    Command::new("stream", "Stream gateway events")
                        .usage("wokhei gateway stream [--follow]")
                        .handler(|_req, _ctx| {
                            Box::pin(async move {
                                Ok(CommandOutput::new(json!({
                                    "stream": "ready"
                                })))
                            })
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
        // Framework usage errors carry the typed USAGE exit code (2), not the
        // generic error code (1), so agents can distinguish bad invocations.
        assert_eq!(run.exit_code(), ExitCode::USAGE);
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
            Command::new("publish", "Publish JSON event").handler(|_req, _ctx| {
                Box::pin(async move {
                    Err(CommandError::new(
                        "invalid event json",
                        "INVALID_EVENT",
                        "Pass valid event JSON and required tags.",
                    ))
                })
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

    // --- Flag parser tests ---

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
    fn bare_long_flag_consumes_next_token_as_value() {
        // Without a boolean-flag schema, `--key value` is treated like
        // `--key=value`. This matches the HATEOAS `[--flag <value>]` form.
        let invocation = parse_invocation(["app", "--title", "My Title"]).expect("should parse");
        assert_eq!(invocation.flag("title"), Some("My Title"));
        assert!(invocation.positionals().is_empty());
    }

    #[test]
    fn bare_short_flag_consumes_next_token_as_value() {
        let invocation = parse_invocation(["app", "-o", "file.txt"]).expect("should parse");
        assert_eq!(invocation.flag("o"), Some("file.txt"));
        assert!(invocation.positionals().is_empty());
    }

    #[test]
    fn bare_long_flag_at_end_of_args_is_boolean() {
        let invocation = parse_invocation(["app", "submit", "--no-git"]).expect("should parse");
        assert_eq!(invocation.flag("no-git"), Some("true"));
        assert_eq!(invocation.positionals(), &[String::from("submit")]);
    }

    #[test]
    fn bare_long_flag_followed_by_flag_is_boolean() {
        // `--verbose --debug` resolves both as bool — `--debug` starts another flag.
        let invocation = parse_invocation(["app", "--verbose", "--debug"]).expect("should parse");
        assert_eq!(invocation.flag("verbose"), Some("true"));
        assert_eq!(invocation.flag("debug"), Some("true"));
    }

    #[test]
    fn schema_aware_bool_flag_does_not_swallow_positional() {
        // When the caller knows which flags are boolean, `--no-git path` keeps
        // `path` as a positional instead of consuming it as the flag's value.
        let bools: HashSet<String> = ["no-git".to_string()].into_iter().collect();
        let invocation =
            parse_invocation_with_bool_flags(["app", "submit", "--no-git", "./plan.html"], |f| {
                bools.contains(f)
            })
            .expect("should parse");
        assert_eq!(invocation.flag("no-git"), Some("true"));
        assert_eq!(
            invocation.positionals(),
            &[String::from("submit"), String::from("./plan.html")]
        );
    }

    #[tokio::test]
    async fn run_argv_honors_usage_string_bool_flags() {
        // `submit --no-git ./plan.html` must NOT consume `./plan.html` as the
        // flag value, because the command's usage declares `--no-git` as bool.
        let cli = AgentCli::new("agplan", "Plan submission tool").command(
            Command::new("submit", "Submit a plan")
                .usage("agplan submit [path] [--title=<title>] [--no-git]")
                .handler(|req, _ctx| {
                    let path = req.arg(0).unwrap_or("").to_string();
                    let no_git = req.flag("no-git").is_some();
                    Box::pin(async move {
                        Ok(CommandOutput::new(json!({
                            "path": path,
                            "no_git": no_git,
                        })))
                    })
                }),
        );

        let run = cli
            .run_argv(["agplan", "submit", "--no-git", "./plan.html"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["path"], json!("./plan.html"));
        assert_eq!(envelope.result["no_git"], json!(true));
    }

    #[tokio::test]
    async fn run_argv_scopes_bool_flags_to_resolved_command() {
        // Regression: when two sibling commands declare the same flag name
        // with conflicting semantics — `cmd_a` treats `--format` as bool,
        // `cmd_b` takes it with a value — invoking `cmd_b --format json`
        // must parse `json` as the flag's value, not as a positional. A
        // global-union bool-flag set would incorrectly mark `--format` as
        // bool everywhere and silently corrupt `cmd_b`'s arguments.
        let cli = AgentCli::new("app", "Conflicting flag schemas")
            .command(
                Command::new("cmd_a", "Bool format")
                    .usage("app cmd_a [--format]")
                    .handler(|_req, _ctx| {
                        Box::pin(async move { Ok(CommandOutput::new(json!({}))) })
                    }),
            )
            .command(
                Command::new("cmd_b", "Value format")
                    .usage("app cmd_b [--format=<format>]")
                    .handler(|req, _ctx| {
                        let format = req.flag("format").unwrap_or("").to_string();
                        let positionals: Vec<String> = req.positionals().to_vec();
                        Box::pin(async move {
                            Ok(CommandOutput::new(json!({
                                "format": format,
                                "positionals": positionals,
                            })))
                        })
                    }),
            );

        let run = cli.run_argv(["app", "cmd_b", "--format", "json"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope, got: {:?}", run.envelope());
        };
        assert_eq!(envelope.result["format"], json!("json"));
        // `json` belongs to the flag, not to the remaining positionals.
        assert_eq!(envelope.result["positionals"], json!([] as [String; 0]));
    }

    #[tokio::test]
    async fn run_argv_still_honors_bool_flag_when_command_declares_it() {
        // Counterpart to `run_argv_scopes_bool_flags_to_resolved_command`:
        // invoking the sibling that *did* declare `--format` as bool must
        // still treat the next positional as a positional, not a flag value.
        let cli = AgentCli::new("app", "Conflicting flag schemas")
            .command(
                Command::new("cmd_a", "Bool format")
                    .usage("app cmd_a [path] [--format]")
                    .handler(|req, _ctx| {
                        let format = req.flag("format").map(|v| v.to_string());
                        let path = req.arg(0).unwrap_or("").to_string();
                        Box::pin(async move {
                            Ok(CommandOutput::new(json!({
                                "format": format,
                                "path": path,
                            })))
                        })
                    }),
            )
            .command(
                Command::new("cmd_b", "Value format")
                    .usage("app cmd_b [--format=<format>]")
                    .handler(|_req, _ctx| {
                        Box::pin(async move { Ok(CommandOutput::new(json!({}))) })
                    }),
            );

        let run = cli
            .run_argv(["app", "cmd_a", "--format", "./payload.json"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope, got: {:?}", run.envelope());
        };
        assert_eq!(envelope.result["format"], json!("true"));
        assert_eq!(envelope.result["path"], json!("./payload.json"));
    }

    #[tokio::test]
    async fn run_argv_supports_space_separated_value_flags() {
        // `submit ./plan.html --title "My Title"` must capture `My Title` as
        // the title value, not the string "true".
        let cli = AgentCli::new("agplan", "Plan submission tool").command(
            Command::new("submit", "Submit a plan")
                .usage("agplan submit [path] [--title=<title>]")
                .handler(|req, _ctx| {
                    let title = req.flag("title").unwrap_or("").to_string();
                    Box::pin(async move { Ok(CommandOutput::new(json!({ "title": title }))) })
                }),
        );

        let run = cli
            .run_argv(["agplan", "submit", "./plan.html", "--title", "My Title"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["title"], json!("My Title"));
    }

    #[test]
    fn extract_bool_flag_names_basic() {
        let mut set = HashSet::new();
        extract_bool_flag_names(
            "agplan submit [path] [--title=<title>] [--no-git] [--dry-run]",
            &mut set,
        );
        assert!(set.contains("no-git"));
        assert!(set.contains("dry-run"));
        assert!(!set.contains("title"));
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

    // --- did-you-mean / inline-valid-names on unknown command|subcommand ---

    #[tokio::test]
    async fn unknown_subcommand_fix_inlines_valid_names() {
        let cli = sample_cli();
        // `list` is a semantic miss (no edit-distance relation to `stream`), so
        // no nudge — but the valid subcommand must be inlined so the agent
        // corrects in one read instead of guessing again.
        let run = cli.run_argv(["wokhei", "gateway", "list"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, "UNKNOWN_SUBCOMMAND");
        assert!(
            envelope.fix.contains("Valid subcommands: stream."),
            "fix should inline valid subcommands, got: {}",
            envelope.fix
        );
        assert!(!envelope.fix.contains("Did you mean"));
    }

    #[tokio::test]
    async fn unknown_subcommand_fix_suggests_typo() {
        let cli = sample_cli();
        let run = cli.run_argv(["wokhei", "gateway", "streem"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, "UNKNOWN_SUBCOMMAND");
        assert!(
            envelope.fix.contains("Did you mean `stream`?"),
            "fix should nudge the nearest name, got: {}",
            envelope.fix
        );
    }

    #[tokio::test]
    async fn unknown_command_fix_inlines_valid_names() {
        let cli = sample_cli();
        let run = cli.run_argv(["wokhei", "bogus"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, "UNKNOWN_COMMAND");
        assert!(envelope.fix.contains("Valid commands:"));
        assert!(envelope.fix.contains("gateway"));
        assert!(envelope.fix.contains("status"));
    }

    #[test]
    fn nearest_name_gates_unrelated_words() {
        // typo within distance 2 → suggested
        assert_eq!(
            nearest_name("streem", &["stream", "status"]),
            Some("stream".to_string())
        );
        // semantic miss, far from any real name → no suggestion
        assert_eq!(nearest_name("list", &["stream", "status"]), None);
        // empty candidate set → no suggestion
        assert_eq!(nearest_name("x", &[]), None);
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

    // --- Typed exit codes ---

    fn rich_cli() -> AgentCli {
        AgentCli::new("app", "Rich output CLI").command(
            Command::new("get", "Get a record")
                .usage("app get <id>")
                .handler(|_req, _ctx| {
                    Box::pin(async move {
                        Ok(CommandOutput::new(json!({
                            "id": 1,
                            "name": "widget",
                            "note": null,
                            "tags": [],
                            "body": "a very long body field"
                        })))
                    })
                }),
        )
    }

    #[tokio::test]
    async fn handler_error_exit_code_is_honored() {
        let cli = AgentCli::new("app", "x").command(Command::new("find", "find").handler(
            |_req, _ctx| {
                Box::pin(async move {
                    Err(CommandError::new("nope", "NOT_FOUND", "Check the id")
                        .exit_code(ExitCode::NOT_FOUND))
                })
            },
        ));
        let run = cli.run_argv(["app", "find"]).await;
        assert_eq!(run.exit_code(), ExitCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn handler_error_without_exit_code_defaults_to_one() {
        let cli = AgentCli::new("app", "x").command(Command::new("boom", "boom").handler(
            |_req, _ctx| Box::pin(async move { Err(CommandError::new("kaboom", "BOOM", "retry")) }),
        ));
        let run = cli.run_argv(["app", "boom"]).await;
        assert_eq!(run.exit_code(), ExitCode::ERROR);
    }

    #[tokio::test]
    async fn parse_error_carries_usage_exit_code() {
        let cli = rich_cli();
        // A lone `--` is fine; `--=x` is invalid flag syntax → PARSE_ERROR.
        let run = cli.run_argv(["app", "--=oops"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, "PARSE_ERROR");
        assert_eq!(run.exit_code(), ExitCode::USAGE);
    }

    // --- Reserved output flags: --select / --compact / --quiet ---

    #[tokio::test]
    async fn select_projects_result_fields() {
        let cli = rich_cli();
        let run = cli
            .run_argv(["app", "get", "1", "--select", "id,name"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result, json!({ "id": 1, "name": "widget" }));
    }

    #[tokio::test]
    async fn compact_drops_null_and_empty_fields() {
        let cli = rich_cli();
        let run = cli.run_argv(["app", "get", "1", "--compact"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        // note (null) and tags ([]) are dropped; the rest remain.
        assert_eq!(
            envelope.result,
            json!({ "id": 1, "name": "widget", "body": "a very long body field" })
        );
    }

    #[tokio::test]
    async fn compact_honors_high_gravity_allowlist() {
        let cli =
            AgentCli::new("app", "x").command(Command::new("get", "get").handler(|_req, _ctx| {
                Box::pin(async move {
                    Ok(
                        CommandOutput::new(json!({ "id": 1, "name": "x", "body": "huge" }))
                            .compact_fields(["id", "name"]),
                    )
                })
            }));
        let run = cli.run_argv(["app", "get", "--compact"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result, json!({ "id": 1, "name": "x" }));
    }

    #[tokio::test]
    async fn quiet_drops_next_actions() {
        let cli = rich_cli();
        let run = cli.run_argv(["app", "get", "1", "--quiet"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.next_actions.is_empty());
    }

    #[tokio::test]
    async fn reserved_flags_can_be_disabled() {
        // With reserved flags off, --select is just an ordinary flag and the
        // result passes through untouched.
        let cli = rich_cli().reserved_flags(false);
        let run = cli.run_argv(["app", "get", "1", "--select", "id"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.result.get("body").is_some());
    }

    // --- Standard flag accessors ---

    #[tokio::test]
    async fn standard_flag_accessors_read_convention_flags() {
        let cli =
            AgentCli::new("app", "x").command(Command::new("run", "run").handler(|req, _ctx| {
                let flags = json!({
                    "dry_run": req.dry_run(),
                    "assume_yes": req.assume_yes(),
                    "no_cache": req.no_cache(),
                    "wants_stdin": req.wants_stdin(),
                });
                Box::pin(async move { Ok(CommandOutput::new(flags)) })
            }));
        let run = cli
            .run_argv([
                "app",
                "run",
                "--dry-run",
                "--no-input",
                "--no-cache",
                "--stdin",
            ])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["dry_run"], json!(true));
        assert_eq!(envelope.result["assume_yes"], json!(true)); // --no-input aliases --yes
        assert_eq!(envelope.result["no_cache"], json!(true));
        assert_eq!(envelope.result["wants_stdin"], json!(true));
    }

    #[tokio::test]
    async fn reserved_bool_flag_does_not_swallow_positional() {
        // `--dry-run` is a framework-reserved bool, so `get --dry-run 1` keeps
        // `1` as the positional id even though `--dry-run` precedes it.
        let cli =
            AgentCli::new("app", "x").command(Command::new("get", "get").handler(|req, _ctx| {
                let id = req.arg(0).unwrap_or("none").to_string();
                let dry = req.dry_run();
                Box::pin(async move { Ok(CommandOutput::new(json!({ "id": id, "dry": dry }))) })
            }));
        let run = cli.run_argv(["app", "get", "--dry-run", "1"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result, json!({ "id": "1", "dry": true }));
    }

    // --- Bounded list output ---

    #[tokio::test]
    async fn list_truncated_emits_bounded_envelope() {
        let cli =
            AgentCli::new("app", "x").command(Command::new("ls", "list").handler(|_req, _ctx| {
                Box::pin(
                    async move { Ok(CommandOutput::list_truncated(vec![json!({ "id": 1 })], 50)) },
                )
            }));
        let run = cli.run_argv(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["count"], json!(1));
        assert_eq!(envelope.result["total"], json!(50));
        assert_eq!(envelope.result["truncated"], json!(true));
        assert!(envelope.result["guidance"].is_string());
    }

    #[tokio::test]
    async fn list_full_has_no_guidance() {
        let items = vec![json!({ "id": 1 }), json!({ "id": 2 })];
        let cli = AgentCli::new("app", "x").command(Command::new("ls", "list").handler(
            move |_req, _ctx| {
                let items = items.clone();
                Box::pin(async move { Ok(CommandOutput::list(items)) })
            },
        ));
        let run = cli.run_argv(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["truncated"], json!(false));
        assert!(envelope.result.get("guidance").is_none());
    }

    // --- Static audit ---

    #[test]
    fn audit_clean_cli_is_clean() {
        let cli = sample_cli();
        let report = cli.audit();
        assert!(
            report.is_clean(),
            "unexpected findings: {:?}",
            report.findings
        );
    }

    #[test]
    fn audit_flags_dangling_next_action() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("a", "Command A")
                .usage("app a")
                .handler(|_req, _ctx| Box::pin(async move { Ok(CommandOutput::new(json!({}))) }))
                .default_next_action(NextAction::new("app nonexistent", "go nowhere")),
        );
        let report = cli.audit();
        assert!(!report.is_clean());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == "DANGLING_NEXT_ACTION")
        );
    }

    #[test]
    fn audit_flags_dead_end_command() {
        let cli = AgentCli::new("app", "x").command(Command::new("orphan", "no handler, no subs"));
        let report = cli.audit();
        assert!(report.findings.iter().any(|f| f.code == "DEAD_END_COMMAND"));
        assert!(!report.is_clean());
    }

    // --- Doctor ---

    #[tokio::test]
    async fn doctor_reports_healthy_when_all_checks_pass() {
        let cli = AgentCli::new("app", "x").doctor(vec![Check::new("ping", || {
            Box::pin(async { crate::CheckResult::pass_with("ok") })
        })]);
        let run = cli.run_argv(["app", "doctor"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["healthy"], json!(true));
        assert_eq!(run.exit_code(), ExitCode::SUCCESS);
    }

    #[tokio::test]
    async fn doctor_failing_check_sets_exit_code() {
        let cli = AgentCli::new("app", "x").doctor(vec![
            Check::new("auth", || {
                Box::pin(async { crate::CheckResult::fail("no token", "Set API_TOKEN") })
            })
            .exit_code(ExitCode::AUTH),
        ]);
        let run = cli.run_argv(["app", "doctor"]).await;
        // Still an ok:true envelope — the report succeeded — but exit code 4.
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["healthy"], json!(false));
        assert_eq!(run.exit_code(), ExitCode::AUTH);
    }

    #[tokio::test]
    async fn doctor_prefers_specific_exit_code_over_generic_error() {
        // A generic-ERROR check registered before a typed check must not mask
        // the more actionable code: priority picks AUTH over ERROR regardless
        // of registration order.
        let cli = AgentCli::new("app", "x").doctor(vec![
            Check::new("connectivity", || {
                Box::pin(async { crate::CheckResult::fail("down", "Check the network") })
            }),
            Check::new("auth", || {
                Box::pin(async { crate::CheckResult::fail("no token", "Set API_TOKEN") })
            })
            .exit_code(ExitCode::AUTH),
        ]);
        let run = cli.run_argv(["app", "doctor"]).await;
        assert_eq!(run.exit_code(), ExitCode::AUTH);
    }

    #[tokio::test]
    async fn doctor_select_does_not_strip_checks_and_fix() {
        // Reserved projection is exempt for doctor: narrowing must never hide
        // the per-check `fix` on an unhealthy report.
        let cli = AgentCli::new("app", "x").doctor(vec![
            Check::new("auth", || {
                Box::pin(async { crate::CheckResult::fail("no token", "Set API_TOKEN") })
            })
            .exit_code(ExitCode::AUTH),
        ]);
        let run = cli.run_argv(["app", "doctor", "--select=healthy"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.result["checks"].is_array());
        assert_eq!(envelope.result["checks"][0]["fix"], json!("Set API_TOKEN"));
        assert_eq!(run.exit_code(), ExitCode::AUTH);
    }

    // --- Handler panic guard ---

    #[tokio::test]
    async fn handler_panic_returns_json_error_envelope() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("boom", "boom")
                .usage("app boom")
                .handler(|_req, _ctx| {
                    Box::pin(async move {
                        // Stand in for any unwrap/expect/index panic in handler code.
                        panic!("simulated handler bug");
                        #[allow(unreachable_code)]
                        Ok(CommandOutput::new(json!({})))
                    })
                }),
        );
        // Silence the default panic hook for this catch_unwind (nextest isolates
        // each test in its own process, so this does not leak to other tests).
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let run = cli.run_argv(["app", "boom"]).await;
        std::panic::set_hook(prev);

        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope, got: {:?}", run.envelope());
        };
        assert_eq!(envelope.error.code, "HANDLER_PANIC");
        assert!(envelope.error.message.contains("handler panicked"));
        assert!(!envelope.fix.is_empty());
        assert!(!envelope.next_actions.is_empty());
        assert_eq!(run.exit_code(), ExitCode::ERROR);
    }

    // --- --select projection cluster: never silently wipe a result ---

    #[tokio::test]
    async fn bare_select_keeps_result_and_warns() {
        // `get 1 --select` (bare, flushed to the "true" sentinel) must not wipe
        // the result to {}; it returns the full result plus a select_warning.
        let cli = rich_cli();
        let run = cli.run_argv(["app", "get", "1", "--select"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["id"], json!(1));
        assert!(envelope.result["select_warning"].is_string());
    }

    #[tokio::test]
    async fn typod_select_keeps_result_and_lists_fields() {
        let cli = rich_cli();
        let run = cli.run_argv(["app", "get", "1", "--select=naem"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["name"], json!("widget"));
        let warning = envelope.result["select_warning"]
            .as_str()
            .expect("warning string");
        assert!(
            warning.contains("name"),
            "warning should list fields: {warning}"
        );
    }

    #[tokio::test]
    async fn valid_select_has_no_warning() {
        let cli = rich_cli();
        let run = cli.run_argv(["app", "get", "1", "--select=id,name"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result, json!({ "id": 1, "name": "widget" }));
        assert!(envelope.result.get("select_warning").is_none());
    }

    // --- Typed argument helpers ---

    #[tokio::test]
    async fn arg_parse_helpers_fold_missing_and_invalid() {
        let cli = AgentCli::new("calc", "x").command(
            Command::new("add", "add")
                .usage("calc add <a> <b>")
                .handler(|req, _ctx| {
                    let a = req.arg_parse::<f64>(0, "a");
                    let b = req.arg_parse::<f64>(1, "b");
                    Box::pin(async move {
                        let sum = a? + b?;
                        Ok(CommandOutput::new(json!({ "sum": sum })))
                    })
                }),
        );
        let run = cli.run_argv(["calc", "add", "3", "5"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["sum"], json!(8.0));

        let run = cli.run_argv(["calc", "add", "3"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, "MISSING_ARG");

        let run = cli.run_argv(["calc", "add", "x", "5"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, "INVALID_ARG");
    }

    // --- Reserved flags discoverable from the root tree ---

    #[tokio::test]
    async fn root_tree_lists_agent_flags() {
        let cli = rich_cli();
        let run = cli.run_argv(["app"]).await;
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
        let cli = rich_cli().reserved_flags(false);
        let run = cli.run_argv(["app"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.result.get("agent_flags").is_none());
    }

    #[test]
    fn reserved_flag_names_includes_select_and_bools() {
        let names = reserved_flag_names();
        assert!(names.contains(&"select"));
        assert!(names.contains(&"quiet"));
        assert!(names.contains(&"dry-run"));
    }

    // --- next_action_from_usage: short value flags & bare optional positionals ---

    #[test]
    fn next_action_from_usage_short_value_flag_is_optional() {
        let action = next_action_from_usage("app log [-v <level>] <file>", "Log a file");
        let params = action.params.as_ref().expect("params should be present");
        assert_eq!(params.get("level").unwrap().required, Some(false));
        assert_eq!(params.get("file").unwrap().required, Some(true));
    }

    #[test]
    fn next_action_from_usage_bare_optional_positional() {
        let action = next_action_from_usage("app x [<optional>]", "X");
        let params = action.params.as_ref().expect("params should be present");
        assert_eq!(params.get("optional").unwrap().required, Some(false));
    }

    // --- Audit: leading placeholder/flag next_action is a dead link ---

    #[test]
    fn audit_flags_next_action_leading_with_placeholder() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("a", "Command A")
                .usage("app a")
                .handler(|_req, _ctx| Box::pin(async move { Ok(CommandOutput::new(json!({}))) }))
                .default_next_action(NextAction::new("<id>", "names no command")),
        );
        let report = cli.audit();
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == "DANGLING_NEXT_ACTION")
        );
    }

    #[test]
    fn audit_allows_bare_root_next_action() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("a", "Command A")
                .usage("app a")
                .handler(|_req, _ctx| Box::pin(async move { Ok(CommandOutput::new(json!({}))) }))
                .default_next_action(NextAction::new("app", "Inspect the root tree")),
        );
        assert!(cli.audit().is_clean(), "{:?}", cli.audit().findings);
    }
}
