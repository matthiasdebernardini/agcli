//! [`Invocation`]: argv parsed into program, flags, and positionals.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::iter;
use std::path::Path;

use crate::redaction::{self, MARKER, RedactFn};
use crate::usage;

/// A command line parsed into the program, its flags and its positionals, with
/// the re-runnable [`Invocation::command_line`] the envelopes carry.
///
/// A handler reads it through
/// [`CommandRequest::invocation`](crate::CommandRequest::invocation). Code
/// that parses argv outside an [`AgentCli`](crate::AgentCli) builds one with
/// [`Invocation::parse`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    program: String,
    command_line: String,
    raw_args: Vec<String>,
    flags: HashMap<String, String>,
    /// The flags given with no value, which `flags` holds as `"true"`. A
    /// value flag among them (`--select` alone) was given nothing, which is
    /// not the same as being given the value `true`.
    bare_flags: HashSet<String>,
    positionals: Vec<String>,
    /// For each entry of `positionals`, its index in `raw_args`. Recovering
    /// this after the fact by searching `raw_args` for the token does not
    /// work: a flag's value can be spelled exactly like a later positional,
    /// so the first textual match is not always the right one.
    positional_indices: Vec<usize>,
    help_requested: bool,
    /// What each token of `raw_args` is, so a redaction knows which tokens
    /// are flag values and which are positionals.
    roles: Vec<Role>,
    /// Each token of `raw_args` as the shell word the `command` line shows.
    shown_args: Vec<String>,
    /// The flags whose value the `command` line and error text mask.
    masked_flags: HashSet<String>,
    /// The positionals (by index in `positionals`) they mask.
    masked_positionals: HashSet<usize>,
}

/// What one token of argv is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Role {
    /// `--`, a bundle of short flags, or a token that was never read.
    Plain,
    /// A flag token: `--key`, `--key=value`, `-k` or `-k=value`. Holds the
    /// name.
    Flag(String),
    /// The token that was the value of the bare flag it follows. Holds the
    /// flag's name.
    FlagValue(String),
    /// A positional. Holds its index among the positionals.
    Positional(usize),
}

impl Invocation {
    /// The program's file name: `argv[0]` without its directory.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The invocation as one shell line: the program's file name, then every
    /// argument, each single-quoted (POSIX style) unless it is made only of
    /// shell-safe characters. The envelope's `command` field.
    ///
    /// [`Invocation::parse`] gives the line verbatim. Under an
    /// [`AgentCli`](crate::AgentCli) the line is redacted, so a handler reads
    /// what the envelope shows: the value of a flag the command does not
    /// declare, of a [`Command::secret`](crate::Command::secret) flag or
    /// positional, and any argument the
    /// [`redact_arg`](crate::AgentCli::redact_arg) hook rewrites, are
    /// replaced by `<redacted>`.
    #[must_use]
    pub fn command_line(&self) -> &str {
        &self.command_line
    }

    /// Every argument after the program, as given.
    #[must_use]
    pub fn raw_args(&self) -> &[String] {
        &self.raw_args
    }

    /// Every flag by name, without its dashes, with its value. A boolean
    /// given bare holds `"true"`.
    #[must_use]
    pub fn flags(&self) -> &HashMap<String, String> {
        &self.flags
    }

    /// The value of flag `name` (without dashes), or `None` when it is
    /// absent.
    #[must_use]
    pub fn flag(&self, name: &str) -> Option<&str> {
        self.flags.get(name).map(String::as_str)
    }

    /// The value of a flag that takes one, or the empty string when the flag
    /// was given with no value (`--select` alone, which [`Invocation::flag`]
    /// reads as `"true"`). `None` when it is absent.
    pub(crate) fn value_flag(&self, name: &str) -> Option<&str> {
        let value = self.flag(name)?;
        Some(if self.bare_flags.contains(name) {
            ""
        } else {
            value
        })
    }

    /// The positional arguments in order, the command path first.
    #[must_use]
    pub fn positionals(&self) -> &[String] {
        &self.positionals
    }

    /// Where each positional sat in [`Invocation::raw_args`], same order and
    /// length as [`Invocation::positionals`]. Maps a positional back to the
    /// original argv position, which is how a raw command finds the exact
    /// token its own argv starts after.
    pub(crate) fn positional_indices(&self) -> &[usize] {
        &self.positional_indices
    }

    /// The command line with `flag` added as a flag token: before the first
    /// `--`, so a re-run parses it as a flag, else at the end. `flag` is
    /// rendered as given, not quoted, so a template placeholder such as
    /// `--select=<fields>` stays one.
    pub(crate) fn command_line_with_flag(&self, flag: &str) -> String {
        let separator = self
            .raw_args
            .iter()
            .position(|token| token == "--")
            .unwrap_or(self.raw_args.len());
        let (before, after) = self.shown_args.split_at(separator);
        iter::once(shell_quote(&self.program).into_owned())
            .chain(before.iter().cloned())
            .chain(iter::once(flag.to_string()))
            .chain(after.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Mask what must not be echoed, and rebuild the `command` line from the
    /// masked tokens. `mask_flag(name)` says whether the value of flag `name`
    /// is masked, `mask_positional(index)` whether positional `index` (in
    /// [`Invocation::positionals`]) is, and `hook` rewrites any token that
    /// neither masks. A flag or positional the hook rewrites counts as masked.
    pub(crate) fn redact(
        &mut self,
        hook: Option<&RedactFn>,
        mask_flag: impl Fn(&str) -> bool,
        mask_positional: impl Fn(usize) -> bool,
    ) {
        let shown: Vec<(String, bool)> = self
            .raw_args
            .iter()
            .zip(&self.roles)
            .map(|(token, role)| match role {
                Role::Flag(name) if mask_flag(name) => (redaction::masked_flag_word(token), true),
                Role::FlagValue(name) if mask_flag(name) => (MARKER.to_string(), true),
                Role::Positional(index) if mask_positional(*index) => (MARKER.to_string(), true),
                _ => redaction::hooked_word(hook, token),
            })
            .collect();
        self.masked_flags.clear();
        self.masked_positionals.clear();
        for (role, (_, masked)) in self.roles.iter().zip(&shown) {
            match role {
                Role::Flag(name) | Role::FlagValue(name) if *masked => {
                    self.masked_flags.insert(name.clone());
                }
                Role::Positional(index) if *masked => {
                    self.masked_positionals.insert(*index);
                }
                _ => {}
            }
        }
        self.shown_args = shown.into_iter().map(|(word, _)| word).collect();
        self.command_line = self.line_of(&self.shown_args);
    }

    /// Replace the `command` line, for a line the framework does not parse.
    pub(crate) fn set_command_line(&mut self, command_line: String) {
        self.command_line = command_line;
    }

    /// The program, then `words`, as one shell line.
    fn line_of(&self, words: &[String]) -> String {
        iter::once(shell_quote(&self.program).into_owned())
            .chain(words.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `value`, the value of flag `name`, as error text may show it: masked
    /// when the flag is.
    pub(crate) fn shown_flag_value<'a>(&self, name: &str, value: &'a str) -> &'a str {
        if self.masked_flags.contains(name) {
            MARKER
        } else {
            value
        }
    }

    /// Positional `index`, as error text may show it: masked when it is.
    pub(crate) fn shown_positional(&self, index: usize) -> &str {
        if self.masked_positionals.contains(&index) {
            MARKER
        } else {
            &self.positionals[index]
        }
    }

    /// True when a bare `--help` or `-h` is on the line. A `--help=<value>`
    /// is read as the `help` flag instead.
    #[must_use]
    pub fn help_requested(&self) -> bool {
        self.help_requested
    }
}

/// Why [`Invocation::parse`] refused argv. The framework answers each with a
/// `PARSE_ERROR` envelope whose `fix` is written for its cause.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseInvocationError {
    /// argv held no program name.
    EmptyArgv,
    /// A flag token with no usable name (`--=value`), or bundled short flags
    /// carrying a value (`-abc=5`). Holds the token.
    InvalidFlag(String),
    /// The same flag given twice (`--select=a --select=b`). Holds its name.
    RepeatedFlag(String),
}

impl fmt::Display for ParseInvocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyArgv => write!(f, "argv is empty"),
            Self::InvalidFlag(flag) => write!(f, "invalid flag syntax: {flag}"),
            Self::RepeatedFlag(name) => write!(f, "{} given twice", usage::flag_display(name)),
        }
    }
}

impl Error for ParseInvocationError {}

impl ParseInvocationError {
    /// This error with the value its token carries after `=` masked, for an
    /// envelope: the token is malformed, but what it carries may be a secret.
    pub(crate) fn redacted(&self) -> Self {
        match self {
            Self::InvalidFlag(token) => Self::InvalidFlag(redaction::mask_flag_value(token)),
            other => other.clone(),
        }
    }

    /// The `fix` of the `PARSE_ERROR` envelope reporting this failure: how
    /// to spell the line so it parses.
    pub(crate) fn fix(&self) -> String {
        match self {
            Self::EmptyArgv => "Pass argv with the program name first.".to_string(),
            Self::InvalidFlag(_) => "Spell a long flag `--name` or `--name=value`, and a short \
                                     flag `-n` or `-n=value`. Bundled short flags (`-abc`) take \
                                     no value, so give a valued short flag on its own (`-a \
                                     -b -c=5`)."
                .to_string(),
            Self::RepeatedFlag(name) if name == "select" => {
                "Pass --select once, with every field in one comma-separated list \
                 (`--select=id,name`)."
                    .to_string()
            }
            Self::RepeatedFlag(name) => format!("Pass {} once.", usage::flag_display(name)),
        }
    }
}

/// The file name of `argv0`, the way an envelope names the program. A path
/// whose file name is not UTF-8 or that has none stays as given.
pub(crate) fn program_name(argv0: &str) -> &str {
    Path::new(argv0)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(argv0)
}

/// Quote one argv token so a reassembled command line runs as printed.
/// Tokens made only of shell-safe characters pass through unchanged; anything
/// else is single-quoted, with embedded single quotes escaped the POSIX way
/// (`'\''`). Without this, `--filter 'two words'` would print as
/// `--filter two words`, a different command.
pub(crate) fn shell_quote(token: &str) -> Cow<'_, str> {
    let is_safe = |c: char| c.is_ascii_alphanumeric() || "-_=/.:,@+".contains(c);
    if !token.is_empty() && token.chars().all(is_safe) {
        return Cow::Borrowed(token);
    }
    Cow::Owned(format!("'{}'", token.replace('\'', r"'\''")))
}

/// Render argv as one re-runnable shell line: `program` reduced to its file
/// name, then `args`, every token shell-quoted where it needs to be. Every
/// `command` field the framework writes goes through here, parsed or not.
pub(crate) fn render_command_line<S: AsRef<str>>(program: &str, args: &[S]) -> String {
    iter::once(program_name(program))
        .chain(args.iter().map(AsRef::as_ref))
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

/// True when `s` is a `-` followed by an ASCII digit (`-3`, `-0x10`,
/// `-1,000`) or parses as a negative number (`-.5`, `-1e3`): a positional,
/// never a bundle of short flags. No usage string declares a digit as a flag.
pub(crate) fn looks_like_negative_number(s: &str) -> bool {
    s.strip_prefix('-').is_some_and(|rest| {
        rest.starts_with(|c: char| c.is_ascii_digit())
            || (!rest.is_empty() && rest.parse::<f64>().is_ok())
    })
}

impl Invocation {
    /// Parse argv (program name first) into an `Invocation`, treating any
    /// flag for which `is_bool` returns `true` as a pure boolean that never
    /// consumes the next token.
    ///
    /// Any other bare long flag (`--key`) followed by a non-flag token takes
    /// that token as its value (`--key value` ≡ `--key=value`), the
    /// space-separated form HATEOAS templates like `[--flag <value>]` use.
    /// So `is_bool` is what keeps `--bool-flag positional` from swallowing
    /// the positional: [`AgentCli::run`](crate::AgentCli::run) derives it
    /// from each command's usage string, and a caller parsing argv outside
    /// the `AgentCli` runtime should pass the same knowledge.
    ///
    /// This parse knows no commands, so `--key grep` gives `--key` the value
    /// `grep` even when `grep` is a command. [`AgentCli::run`](crate::AgentCli::run)
    /// knows the command tree, so there a command's name, where one can go,
    /// is never a flag's value: `--key` is given no value and `grep` is the
    /// command.
    ///
    /// # Errors
    ///
    /// - [`ParseInvocationError::EmptyArgv`] if `args` yields no program name.
    /// - [`ParseInvocationError::InvalidFlag`] for a flag with an empty name
    ///   (`--=value`, `-=value`) and for bundled short flags carrying a value
    ///   (`-abc=5`). A lone `--` is not an error: it ends flag parsing.
    /// - [`ParseInvocationError::RepeatedFlag`] for a flag given twice, in any
    ///   spelling (`--n=1 -n 2`, `-vv`): the parser never picks a winner.
    pub fn parse<I, S, F>(args: I, is_bool: F) -> Result<Self, ParseInvocationError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
        F: Fn(&str) -> bool,
    {
        Self::parse_in_tree(args, is_bool, |_, _| false)
    }

    /// [`Invocation::parse`], where `names_command(positionals, token)` says
    /// whether `token`, read after `positionals`, names a command. Such a
    /// token is a positional even right after a bare flag that would take it
    /// as its value: the flag is given no value instead.
    ///
    /// # Errors
    ///
    /// As [`Invocation::parse`].
    pub(crate) fn parse_in_tree<I, S, F, C>(
        args: I,
        is_bool: F,
        names_command: C,
    ) -> Result<Self, ParseInvocationError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
        F: Fn(&str) -> bool,
        C: Fn(&[String], &str) -> bool,
    {
        let mut iter = args.into_iter().map(Into::into);
        let raw_program = iter.next().ok_or(ParseInvocationError::EmptyArgv)?;
        let program = program_name(&raw_program).to_string();
        let tokens: Vec<String> = iter.collect();
        let command_line = render_command_line(&program, &tokens);

        let mut parser = TokenParser::new(tokens.len());
        for (index, token) in tokens.iter().enumerate() {
            parser.feed(index, token, &is_bool, &names_command)?;
        }
        // End of args: any pending bare flag becomes boolean.
        parser.flush_pending()?;

        let shown_args = tokens
            .iter()
            .map(|token| shell_quote(token).into_owned())
            .collect();
        Ok(Self {
            program,
            command_line,
            raw_args: tokens,
            roles: parser.roles,
            shown_args,
            masked_flags: HashSet::new(),
            masked_positionals: HashSet::new(),
            flags: parser.flags,
            bare_flags: parser.bare_flags,
            positionals: parser.positionals,
            positional_indices: parser.positional_indices,
            help_requested: parser.help_requested,
        })
    }
}

/// The token-by-token state of [`Invocation::parse`].
struct TokenParser {
    flags: HashMap<String, String>,
    /// The flags set to `"true"` because they were given no value.
    bare_flags: HashSet<String>,
    positionals: Vec<String>,
    positional_indices: Vec<usize>,
    help_requested: bool,
    /// Set by `--`: every later token is a positional.
    positional_only: bool,
    /// A bare `--key` whose value form we don't yet know. If the next token is
    /// a plain value that names no command, it becomes the flag's value;
    /// otherwise the flag is bool.
    pending_flag: Option<String>,
    /// The role of each token fed so far, by index.
    roles: Vec<Role>,
}

impl TokenParser {
    fn new(token_count: usize) -> Self {
        Self {
            flags: HashMap::new(),
            bare_flags: HashSet::new(),
            positionals: Vec::new(),
            positional_indices: Vec::new(),
            help_requested: false,
            positional_only: false,
            pending_flag: None,
            roles: vec![Role::Plain; token_count],
        }
    }

    /// Consume the argv token at `index`.
    fn feed(
        &mut self,
        index: usize,
        token: &str,
        is_bool: impl Fn(&str) -> bool,
        names_command: impl Fn(&[String], &str) -> bool,
    ) -> Result<(), ParseInvocationError> {
        if self.positional_only {
            self.flush_pending()?;
            self.push_positional(index, token);
            return Ok(());
        }

        match token {
            "--" => {
                self.flush_pending()?;
                self.positional_only = true;
                return Ok(());
            }
            "--help" | "-h" => {
                self.flush_pending()?;
                self.help_requested = true;
                return Ok(());
            }
            _ => {}
        }

        // Long flags: --key=value or bare --key
        if let Some(flag) = token.strip_prefix("--") {
            return self.long_flag(index, token, flag, is_bool);
        }

        // Negative numbers as positionals: -123, -3.14
        if looks_like_negative_number(token) {
            return self.value_or_positional(index, token, names_command);
        }

        // Short flags: -x, -x=value, or -abc (bundled, all bool)
        if token.starts_with('-') && token.len() > 1 {
            return self.short_flag(index, token, &token[1..], is_bool);
        }

        // Plain positional. If a flag is pending, this token is its value,
        // unless it names a command.
        self.value_or_positional(index, token, names_command)
    }

    /// Consume a `--flag` token; `flag` is `token` without the `--`.
    fn long_flag(
        &mut self,
        index: usize,
        token: &str,
        flag: &str,
        is_bool: impl Fn(&str) -> bool,
    ) -> Result<(), ParseInvocationError> {
        if flag.is_empty() {
            return Err(ParseInvocationError::InvalidFlag(token.to_string()));
        }

        // Starting a new flag flushes any prior pending bare flag.
        self.flush_pending()?;

        if let Some((key, value)) = flag.split_once('=') {
            if key.is_empty() {
                return Err(ParseInvocationError::InvalidFlag(token.to_string()));
            }
            self.roles[index] = Role::Flag(key.to_string());
            return self.set_flag(key.to_string(), value.to_string());
        }

        // Bare --flag: known boolean, or candidate for next-token value.
        self.roles[index] = Role::Flag(flag.to_string());
        self.bare_flag(flag, is_bool)
    }

    /// Consume a `-x` token; `short` is `token` without the `-`.
    fn short_flag(
        &mut self,
        index: usize,
        token: &str,
        short: &str,
        is_bool: impl Fn(&str) -> bool,
    ) -> Result<(), ParseInvocationError> {
        // Encountering another flag resolves the pending bare flag as bool.
        self.flush_pending()?;

        // `-k=value`. A value belongs to exactly one flag: `-abc=5` cannot
        // say which of the three it is for, and `-=5` names none.
        if let Some((key, value)) = short.split_once('=') {
            if key.chars().count() != 1 {
                return Err(ParseInvocationError::InvalidFlag(token.to_string()));
            }
            self.roles[index] = Role::Flag(key.to_string());
            return self.set_flag(key.to_string(), value.to_string());
        }

        if short.chars().count() == 1 {
            self.roles[index] = Role::Flag(short.to_string());
            return self.bare_flag(short, is_bool);
        }

        // Bundled short flags: -abc (all bool)
        for ch in short.chars() {
            self.set_bare_flag(ch.to_string())?;
        }
        Ok(())
    }

    /// A flag given without `=value`: a known boolean is set now; any other
    /// waits for the next token to learn whether it has a value.
    fn bare_flag(
        &mut self,
        name: &str,
        is_bool: impl Fn(&str) -> bool,
    ) -> Result<(), ParseInvocationError> {
        if is_bool(name) {
            return self.set_bare_flag(name.to_string());
        }
        if self.flags.contains_key(name) {
            return Err(ParseInvocationError::RepeatedFlag(name.to_string()));
        }
        self.pending_flag = Some(name.to_string());
        Ok(())
    }

    /// Record flag `key`, refusing a second occurrence.
    fn set_flag(&mut self, key: String, value: String) -> Result<(), ParseInvocationError> {
        if self.flags.contains_key(&key) {
            return Err(ParseInvocationError::RepeatedFlag(key));
        }
        self.flags.insert(key, value);
        Ok(())
    }

    /// Record flag `key` as given with no value: `"true"`, and remembered as
    /// bare.
    fn set_bare_flag(&mut self, key: String) -> Result<(), ParseInvocationError> {
        self.bare_flags.insert(key.clone());
        self.set_flag(key, "true".to_string())
    }

    /// A non-flag token: the pending flag's value if one is waiting and the
    /// token names no command, else a positional.
    fn value_or_positional(
        &mut self,
        index: usize,
        token: &str,
        names_command: impl Fn(&[String], &str) -> bool,
    ) -> Result<(), ParseInvocationError> {
        if let Some(key) = self.pending_flag.take() {
            if !names_command(&self.positionals, token) {
                self.roles[index] = Role::FlagValue(key.clone());
                return self.set_flag(key, token.to_string());
            }
            self.set_bare_flag(key)?;
        }
        self.push_positional(index, token);
        Ok(())
    }

    fn push_positional(&mut self, index: usize, token: &str) {
        self.roles[index] = Role::Positional(self.positionals.len());
        self.positionals.push(token.to_string());
        self.positional_indices.push(index);
    }

    /// Resolve a pending bare flag as boolean.
    fn flush_pending(&mut self) -> Result<(), ParseInvocationError> {
        self.pending_flag
            .take()
            .map_or(Ok(()), |key| self.set_bare_flag(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;

    /// Parse with no boolean-flag schema.
    fn parse_no_bools<const N: usize>(args: [&str; N]) -> Result<Invocation, ParseInvocationError> {
        Invocation::parse(args, |_| false)
    }

    /// Parse a slice with `quiet` and `v` as booleans.
    fn parse_with_bools(args: &[&str]) -> Result<Invocation, ParseInvocationError> {
        Invocation::parse(args.iter().copied(), |flag| flag == "quiet" || flag == "v")
    }

    #[test]
    fn parse_invocation_handles_flags_and_positionals() {
        let invocation = parse_no_bools([
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

    // --- New parser edge-case tests ---

    #[test]
    fn negative_number_treated_as_positional() {
        let invocation =
            parse_no_bools(["app", "status", "-123"]).expect("invocation should parse");
        assert_eq!(
            invocation.positionals(),
            &[String::from("status"), String::from("-123")]
        );
        assert!(invocation.flags().is_empty());
    }

    #[test]
    fn negative_float_treated_as_positional() {
        let invocation = parse_no_bools(["app", "-3.14"]).expect("invocation should parse");
        assert_eq!(invocation.positionals(), &[String::from("-3.14")]);
    }

    #[test]
    fn long_flag_with_negative_value() {
        let invocation = parse_no_bools(["app", "--count=-1"]).expect("invocation should parse");
        assert_eq!(invocation.flag("count"), Some("-1"));
        assert!(
            invocation.positionals().is_empty(),
            "expected no positionals, got {:?}",
            invocation.positionals()
        );
    }

    #[test]
    fn short_flag_with_negative_value() {
        let invocation = parse_no_bools(["app", "-n=-5"]).expect("invocation should parse");
        assert_eq!(invocation.flag("n"), Some("-5"));
        assert!(
            invocation.positionals().is_empty(),
            "expected no positionals, got {:?}",
            invocation.positionals()
        );
    }

    #[test]
    fn long_flag_equals_syntax() {
        let invocation = parse_no_bools(["app", "--count=-1"]).expect("invocation should parse");
        assert_eq!(invocation.flag("count"), Some("-1"));
    }

    #[test]
    fn double_dash_forces_positionals() {
        let invocation =
            parse_no_bools(["app", "--", "--not-a-flag", "-abc"]).expect("invocation should parse");
        assert_eq!(
            invocation.positionals(),
            &[String::from("--not-a-flag"), String::from("-abc")]
        );
        assert!(invocation.flags().is_empty());
    }

    #[test]
    fn bundled_short_flags() {
        let invocation = parse_no_bools(["app", "-abc"]).expect("invocation should parse");
        assert_eq!(invocation.flag("a"), Some("true"));
        assert_eq!(invocation.flag("b"), Some("true"));
        assert_eq!(invocation.flag("c"), Some("true"));
    }

    // --- Flag parser tests ---

    #[test]
    fn long_flag_equals_carries_value() {
        let invocation = parse_no_bools(["app", "--output=file.txt"]).expect("should parse");
        assert_eq!(invocation.flag("output"), Some("file.txt"));
        assert!(
            invocation.positionals().is_empty(),
            "expected no positionals, got {:?}",
            invocation.positionals()
        );
    }

    #[test]
    fn short_flag_equals_carries_value() {
        let invocation = parse_no_bools(["app", "-o=file.txt"]).expect("should parse");
        assert_eq!(invocation.flag("o"), Some("file.txt"));
        assert!(
            invocation.positionals().is_empty(),
            "expected no positionals, got {:?}",
            invocation.positionals()
        );
    }

    #[test]
    fn bare_long_flag_consumes_next_token_as_value() {
        // Without a boolean-flag schema, `--key value` is treated like
        // `--key=value`. This matches the HATEOAS `[--flag <value>]` form.
        let invocation = parse_no_bools(["app", "--title", "My Title"]).expect("should parse");
        assert_eq!(invocation.flag("title"), Some("My Title"));
        assert!(
            invocation.positionals().is_empty(),
            "expected no positionals, got {:?}",
            invocation.positionals()
        );
    }

    #[test]
    fn bare_short_flag_consumes_next_token_as_value() {
        let invocation = parse_no_bools(["app", "-o", "file.txt"]).expect("should parse");
        assert_eq!(invocation.flag("o"), Some("file.txt"));
        assert!(
            invocation.positionals().is_empty(),
            "expected no positionals, got {:?}",
            invocation.positionals()
        );
    }

    #[test]
    fn bare_long_flag_at_end_of_args_is_boolean() {
        let invocation = parse_no_bools(["app", "submit", "--no-git"]).expect("should parse");
        assert_eq!(invocation.flag("no-git"), Some("true"));
        assert_eq!(invocation.positionals(), &[String::from("submit")]);
    }

    #[test]
    fn bare_long_flag_followed_by_flag_is_boolean() {
        // `--verbose --debug` resolves both as bool: `--debug` starts another flag.
        let invocation = parse_no_bools(["app", "--verbose", "--debug"]).expect("should parse");
        assert_eq!(invocation.flag("verbose"), Some("true"));
        assert_eq!(invocation.flag("debug"), Some("true"));
    }

    #[test]
    fn schema_aware_bool_flag_does_not_swallow_positional() {
        // When the caller knows which flags are boolean, `--no-git path` keeps
        // `path` as a positional instead of consuming it as the flag's value.
        let bools: HashSet<String> = ["no-git".to_string()].into_iter().collect();
        let invocation = Invocation::parse(["app", "submit", "--no-git", "./plan.html"], |f| {
            bools.contains(f)
        })
        .expect("should parse");
        assert_eq!(invocation.flag("no-git"), Some("true"));
        assert_eq!(
            invocation.positionals(),
            &[String::from("submit"), String::from("./plan.html")]
        );
    }

    #[test]
    fn scientific_notation_negative_number() {
        let invocation = parse_no_bools(["app", "-1e3"]).expect("should parse");
        assert_eq!(invocation.positionals(), &[String::from("-1e3")]);
        assert!(invocation.flags().is_empty());
    }

    #[test]
    fn shell_quote_only_quotes_what_needs_it() {
        assert_eq!(shell_quote("--select=items.id"), "--select=items.id");
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote(""), "''");
        // POSIX has no escape inside single quotes: close, escape, reopen.
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("a;rm -rf /"), "'a;rm -rf /'");
    }

    #[test]
    fn bundled_short_flags_cannot_carry_a_value() {
        for token in ["-abc=5", "-ab=", "-=5"] {
            assert_eq!(
                parse_no_bools(["app", token]),
                Err(ParseInvocationError::InvalidFlag(token.to_string())),
                "{token}"
            );
        }
        // One short flag may.
        let invocation = parse_no_bools(["app", "-a=5"]).expect("should parse");
        assert_eq!(invocation.flag("a"), Some("5"));
    }

    #[test]
    fn a_flag_given_twice_is_an_error_naming_it() {
        for argv in [
            &["app", "--select=a", "--select=b"][..],
            &["app", "--select", "a", "--select=b"],
            &["app", "--quiet", "--quiet"],
            &["app", "-n", "1", "-n=2"],
            &["app", "-vv"],
            &["app", "-v", "--v"],
        ] {
            let error = parse_with_bools(argv).expect_err("must refuse a repeat");
            assert!(
                matches!(error, ParseInvocationError::RepeatedFlag(_)),
                "{argv:?}: {error:?}"
            );
        }
        assert_eq!(
            parse_no_bools(["app", "--select=a", "--select=b"])
                .expect_err("repeat")
                .to_string(),
            "--select given twice"
        );
        assert_eq!(
            parse_no_bools(["app", "-vv"])
                .expect_err("repeat")
                .to_string(),
            "-v given twice"
        );
    }

    #[test]
    fn command_line_is_shell_quoted_and_names_the_program_by_file_name() {
        let invocation = parse_no_bools([
            "/usr/local/bin/app",
            "note",
            "two words",
            "it's",
            "--tag=a,b",
        ])
        .expect("should parse");
        assert_eq!(
            invocation.command_line(),
            r"app note 'two words' 'it'\''s' --tag=a,b"
        );
        assert_eq!(render_command_line("/bin/app", &[] as &[&str]), "app");
        assert_eq!(render_command_line("app", &[""]), "app ''");
    }

    #[test]
    fn shorthand_decimal_negative_number() {
        let invocation = parse_no_bools(["app", "-.5"]).expect("should parse");
        assert_eq!(invocation.positionals(), &[String::from("-.5")]);
        assert!(invocation.flags().is_empty());
    }

    #[test]
    fn a_dash_and_a_digit_start_a_positional_not_a_flag_bundle() {
        // `-0x10` and `-1,000` are not f64 literals, so they used to be read
        // as bundled short flags and fail with `-0 given twice`.
        for token in ["-0x10", "-1,000", "-7th", "-2024-01-01"] {
            let invocation = parse_no_bools(["app", "add", "1", token]).expect("should parse");
            assert_eq!(
                invocation.positionals(),
                &[String::from("add"), String::from("1"), String::from(token)],
                "{token}"
            );
            assert!(invocation.flags().is_empty(), "{token}");
        }
        // A letter after the dash is still a flag bundle, and `-` alone a
        // positional.
        let invocation = parse_no_bools(["app", "-ab", "-"]).expect("should parse");
        assert_eq!(invocation.flag("a"), Some("true"));
        assert_eq!(invocation.positionals(), &[String::from("-")]);
    }

    #[test]
    fn a_value_flag_given_bare_reads_as_empty() {
        // `flag` keeps the parser's `"true"`; `value_flag` tells a flag given
        // nothing from one given the value `true`.
        let bare = parse_no_bools(["app", "--select"]).expect("should parse");
        assert_eq!(bare.flag("select"), Some("true"));
        assert_eq!(bare.value_flag("select"), Some(""));
        let before_a_flag = parse_no_bools(["app", "--select", "--quiet"]).expect("should parse");
        assert_eq!(before_a_flag.value_flag("select"), Some(""));
        let given = parse_no_bools(["app", "--select", "true"]).expect("should parse");
        assert_eq!(given.value_flag("select"), Some("true"));
        let equals = parse_no_bools(["app", "--select=true"]).expect("should parse");
        assert_eq!(equals.value_flag("select"), Some("true"));
        assert_eq!(equals.value_flag("other"), None);
        let bundled = parse_no_bools(["app", "-sv"]).expect("should parse");
        assert_eq!(bundled.value_flag("s"), Some(""));
    }

    #[test]
    fn command_line_with_flag_keeps_the_flag_before_the_separator() {
        let invocation =
            parse_no_bools(["/bin/app", "ls", "two words", "--", "-x"]).expect("should parse");
        assert_eq!(
            invocation.command_line_with_flag("--select=<fields>"),
            "app ls 'two words' --select=<fields> -- -x"
        );
        let invocation = parse_no_bools(["app", "ls"]).expect("should parse");
        assert_eq!(
            invocation.command_line_with_flag("--select=<fields>"),
            "app ls --select=<fields>"
        );
    }

    #[test]
    fn each_parse_error_carries_its_own_fix() {
        assert_eq!(
            ParseInvocationError::EmptyArgv.fix(),
            "Pass argv with the program name first."
        );
        let invalid = ParseInvocationError::InvalidFlag("-abc=5".to_string()).fix();
        assert!(invalid.contains("`--name` or `--name=value`"), "{invalid}");
        assert!(
            invalid.contains("give a valued short flag on its own"),
            "{invalid}"
        );
        assert_eq!(
            ParseInvocationError::RepeatedFlag("select".to_string()).fix(),
            "Pass --select once, with every field in one comma-separated list \
             (`--select=id,name`)."
        );
        assert_eq!(
            ParseInvocationError::RepeatedFlag("v".to_string()).fix(),
            "Pass -v once."
        );
    }

    /// Parse with no boolean-flag schema, where `grep` is the only command.
    fn parse_with_grep(args: &[&str]) -> Result<Invocation, ParseInvocationError> {
        Invocation::parse_in_tree(
            args.iter().copied(),
            |_| false,
            |positionals, token| positionals.is_empty() && token == "grep",
        )
    }

    #[test]
    fn a_command_name_is_never_a_flag_value() {
        let bare = parse_with_grep(&["app", "--max", "grep", "pat"]).expect("should parse");
        assert_eq!(bare.value_flag("max"), Some(""));
        assert_eq!(
            bare.positionals(),
            &[String::from("grep"), String::from("pat")]
        );
        assert_eq!(bare.positional_indices(), &[1, 2]);

        let short = parse_with_grep(&["app", "-n", "grep", "pat"]).expect("should parse");
        assert_eq!(short.value_flag("n"), Some(""));
        assert_eq!(
            short.positionals(),
            &[String::from("grep"), String::from("pat")]
        );

        let valued = parse_with_grep(&["app", "--max", "2", "grep"]).expect("should parse");
        assert_eq!(valued.flag("max"), Some("2"));
        assert_eq!(valued.positionals(), &[String::from("grep")]);

        let equals = parse_with_grep(&["app", "--max=grep", "grep"]).expect("should parse");
        assert_eq!(equals.flag("max"), Some("grep"));
        assert_eq!(equals.positionals(), &[String::from("grep")]);

        // Past the first positional, `grep` names no command here, so it is a
        // value again.
        let later = parse_with_grep(&["app", "grep", "--max", "grep"]).expect("should parse");
        assert_eq!(later.flag("max"), Some("grep"));
        assert_eq!(later.positionals(), &[String::from("grep")]);
    }

    #[test]
    fn parse_knows_no_commands() {
        let invocation = parse_no_bools(["app", "--max", "grep", "pat"]).expect("should parse");
        assert_eq!(invocation.flag("max"), Some("grep"));
        assert_eq!(invocation.positionals(), &[String::from("pat")]);
    }

    // --- redaction ---

    fn parse_bool_flags<const N: usize>(args: [&str; N], bools: &[&str]) -> Invocation {
        Invocation::parse(args, |flag| bools.contains(&flag)).expect("should parse")
    }

    #[test]
    fn roles_tell_flag_values_from_positionals() {
        let invocation = parse_no_bools(["app", "--a=1", "--b", "2", "-c", "3", "pos", "--", "-d"])
            .expect("should parse");
        assert_eq!(
            invocation.roles,
            [
                Role::Flag("a".to_string()),
                Role::Flag("b".to_string()),
                Role::FlagValue("b".to_string()),
                Role::Flag("c".to_string()),
                Role::FlagValue("c".to_string()),
                Role::Positional(0),
                Role::Plain,
                Role::Positional(1),
            ]
        );
    }

    #[test]
    fn redact_masks_flags_and_positionals_and_rebuilds_the_line() {
        let mut invocation = parse_bool_flags(
            [
                "app", "run", "--tok", "s3cret", "--keep=1", "pw=x", "-q", "--", "--pw=y",
            ],
            &["q"],
        );
        let hook: &RedactFn = &|arg| arg.starts_with("pw=").then(|| "pw=<r>".to_string());
        invocation.redact(Some(hook), |name| name == "tok", |index| index == 2);
        assert_eq!(
            invocation.command_line(),
            "app run --tok <redacted> --keep=1 'pw=<r>' -q -- <redacted>"
        );
        assert_eq!(invocation.shown_flag_value("tok", "s3cret"), MARKER);
        assert_eq!(invocation.shown_flag_value("keep", "1"), "1");
        assert_eq!(invocation.shown_positional(0), "run");
        assert_eq!(invocation.shown_positional(1), MARKER);
        assert_eq!(invocation.shown_positional(2), MARKER);
        // The real values stay readable.
        assert_eq!(invocation.flag("tok"), Some("s3cret"));
        assert_eq!(invocation.positionals()[1], "pw=x");
        assert_eq!(
            invocation.command_line_with_flag("--select=<fields>"),
            "app run --tok <redacted> --keep=1 'pw=<r>' -q --select=<fields> -- <redacted>"
        );
    }

    #[test]
    fn an_invalid_flag_error_masks_its_value_for_an_envelope() {
        let error = parse_no_bools(["app", "-abc=5"]).expect_err("should not parse");
        assert_eq!(error.to_string(), "invalid flag syntax: -abc=5");
        assert_eq!(
            error.redacted().to_string(),
            "invalid flag syntax: -abc=<redacted>"
        );
        assert_eq!(
            ParseInvocationError::EmptyArgv.redacted(),
            ParseInvocationError::EmptyArgv
        );
    }
}
