//! [`CommandRequest`]: what a handler sees of its invocation, with typed
//! argument and flag accessors.

use std::any::Any;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use crate::clock::ClockFn;
use crate::codes::{ErrorCode, ExitCode};
use crate::command_error::CommandError;
use crate::invocation::Invocation;
use crate::reserved_flags;
use crate::select;
use crate::timestamp::Timestamp;
use crate::usage;

/// What a handler receives: the parsed invocation, where the command path
/// ends in it, and the run's clock.
///
/// Owned and cheap to clone (the invocation sits behind an [`Arc`]), so a
/// handler takes it by value and moves it into its `async move` block:
/// `.handler(|req| async move { let a: f64 = req.arg_parse(0, "a")?; .. })`.
#[derive(Clone)]
pub struct CommandRequest {
    invocation: Arc<Invocation>,
    /// How many leading positionals name the command (`["gateway", "stream"]`
    /// is 2). The rest are the command's own arguments.
    path_len: usize,
    /// The run's clock, for [`CommandRequest::now`].
    clock: Arc<ClockFn>,
}

impl fmt::Debug for CommandRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandRequest")
            .field("invocation", &self.invocation)
            .field("path_len", &self.path_len)
            .finish_non_exhaustive()
    }
}

impl CommandRequest {
    pub(crate) fn new(invocation: Arc<Invocation>, path_len: usize, clock: Arc<ClockFn>) -> Self {
        debug_assert!(
            path_len <= invocation.positionals().len(),
            "command path is longer than the positionals it came from"
        );
        Self {
            invocation,
            path_len,
            clock,
        }
    }

    /// The run's clock, read now: the second to stamp a result with, such as
    /// a `created_at`. It is the clock every `timestamp` and `ts` of the run
    /// comes from, so a test that pins it with
    /// [`AgentCli::clock`](crate::AgentCli::clock) gets byte-stable output
    /// from the handler too.
    #[must_use]
    pub fn now(&self) -> Timestamp {
        (self.clock)()
    }

    /// The whole parsed command line: the program, every flag and every
    /// positional, the command path included.
    #[must_use]
    pub fn invocation(&self) -> &Invocation {
        &self.invocation
    }

    /// The command path that resolved to this handler, e.g. `["gateway", "stream"]`.
    #[must_use]
    pub fn command_path(&self) -> &[String] {
        &self.invocation.positionals()[..self.path_len]
    }

    /// The positionals after the command path: the command's own arguments.
    #[must_use]
    pub fn positionals(&self) -> &[String] {
        &self.invocation.positionals()[self.path_len..]
    }

    /// The command's own positional argument `index` (0 is the first after
    /// the command path), or `None`. [`CommandRequest::require_arg`] and
    /// [`CommandRequest::arg_parse`] turn its absence into an error.
    #[must_use]
    pub fn arg(&self, index: usize) -> Option<&str> {
        self.positionals().get(index).map(String::as_str)
    }

    /// The raw value of flag `key` (without dashes: `"lines"`, `"n"`), or
    /// `None` when it is absent. A boolean given bare reads as `"true"`.
    /// [`CommandRequest::flag_parse`] and [`CommandRequest::flag_on`] read it
    /// typed.
    #[must_use]
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
    ///
    /// # Errors
    ///
    /// `MISSING_ARG` (exit code [`ExitCode::USAGE`]) when there is no
    /// positional at `index`.
    pub fn require_arg(&self, index: usize, name: &str) -> Result<&str, CommandError> {
        self.arg(index).ok_or_else(|| {
            CommandError::new(
                ErrorCode::MISSING_ARG,
                format!("missing argument <{name}>"),
                format!("Provide <{name}> as positional argument {index}."),
            )
            .exit_code(ExitCode::USAGE)
        })
    }

    /// Parse positional `index` into `T`, erroring with `MISSING_ARG` when
    /// absent or `INVALID_ARG` when it does not parse. Non-finite floats
    /// (`"inf"` and `"NaN"`, which `f64::from_str` accepts) are rejected as
    /// `INVALID_ARG` too: `serde_json` serializes them as `null`, so letting
    /// one through yields `ok: true` with a corrupted result.
    ///
    /// # Errors
    ///
    /// All with exit code [`ExitCode::USAGE`]:
    /// - `MISSING_ARG` when there is no positional at `index`.
    /// - `INVALID_ARG` when it does not parse as `T`; the message carries
    ///   `T`'s parse error.
    /// - `INVALID_ARG` when it parses to an infinite or NaN float.
    pub fn arg_parse<T>(&self, index: usize, name: &str) -> Result<T, CommandError>
    where
        T: FromStr + Any,
        T::Err: fmt::Display,
    {
        let raw = self.require_arg(index, name)?;
        let at = self.path_len + index;
        let shown = self.invocation.shown_positional(at);
        let value = raw.parse::<T>().map_err(|err| {
            // A parse error may repeat the input, so a masked value gets none.
            let detail = if self.invocation.positional_is_masked(at) {
                String::new()
            } else {
                format!(" ({err})")
            };
            CommandError::new(
                ErrorCode::INVALID_ARG,
                format!("argument <{name}> is not valid: {shown:?}{detail}"),
                format!("Pass a valid value for <{name}>."),
            )
            .exit_code(ExitCode::USAGE)
        })?;
        if !parsed_value_is_finite(&value) {
            return Err(CommandError::new(
                ErrorCode::INVALID_ARG,
                format!("argument <{name}> is not finite: {shown:?}"),
                format!(
                    "Pass a finite number for <{name}>. JSON cannot represent \
                     infinities or NaN."
                ),
            )
            .exit_code(ExitCode::USAGE));
        }
        Ok(value)
    }

    /// Parse flag `key` into `T` when present. `Ok(None)` when the flag is
    /// absent; `INVALID_FLAG` when given bare, with no value, or present but
    /// unparseable or non-finite (see [`Self::arg_parse`]).
    ///
    /// # Errors
    ///
    /// `INVALID_FLAG` (exit code [`ExitCode::USAGE`]) when the flag is given
    /// with no value (`--limit` alone, which [`Self::flag`] reads as
    /// `"true"`), when it does not parse as `T` (the message carries `T`'s
    /// parse error), or when it parses to an infinite or NaN float.
    pub fn flag_parse<T>(&self, key: &str) -> Result<Option<T>, CommandError>
    where
        T: FromStr + Any,
        T::Err: fmt::Display,
    {
        match self.flag(key) {
            None => Ok(None),
            Some(raw) => {
                let flag = usage::flag_display(key);
                // A bare flag reads as "true" but carries no value.
                if !raw.is_empty() && self.invocation.value_flag(key) == Some("") {
                    return Err(CommandError::new(
                        ErrorCode::INVALID_FLAG,
                        format!("flag {flag} was given no value"),
                        format!("Pass a value: {flag}=<value>"),
                    )
                    .exit_code(ExitCode::USAGE));
                }
                let shown = self.invocation.shown_flag_value(key, raw);
                let value = raw.parse::<T>().map_err(|err| {
                    // A parse error may repeat the input, so a masked value
                    // gets none.
                    let detail = if self.invocation.flag_is_masked(key) {
                        String::new()
                    } else {
                        format!(" ({err})")
                    };
                    CommandError::new(
                        ErrorCode::INVALID_FLAG,
                        format!("flag {flag} is not valid: {shown:?}{detail}"),
                        format!("Pass a valid value for {flag}."),
                    )
                    .exit_code(ExitCode::USAGE)
                })?;
                if !parsed_value_is_finite(&value) {
                    return Err(CommandError::new(
                        ErrorCode::INVALID_FLAG,
                        format!("flag {flag} is not finite: {shown:?}"),
                        format!(
                            "Pass a finite number for {flag}. JSON cannot represent \
                             infinities or NaN."
                        ),
                    )
                    .exit_code(ExitCode::USAGE));
                }
                Ok(Some(value))
            }
        }
    }

    /// Read boolean flag `key` strictly: `Ok(false)` when absent, else the
    /// boolean its value spells (`true`/`1`/`yes`/`on` or
    /// `false`/`0`/`no`/`off`, any ASCII case). A bare `--force` parses as
    /// `"true"`, and `--force=false` reads as off, where
    /// `req.flag("force").is_some()` would read it as on.
    ///
    /// ```
    /// use agcli::{Command, CommandOutput};
    /// use serde_json::json;
    ///
    /// let prune = Command::new("prune", "Delete stale entries")
    ///     .usage("app prune [--force]")
    ///     .handler(|req| async move {
    ///         let force = req.flag_on("force")?;
    ///         Ok(CommandOutput::new(json!({ "force": force })))
    ///     });
    /// # let _ = prune;
    /// ```
    ///
    /// # Errors
    ///
    /// `INVALID_FLAG` (exit code [`ExitCode::USAGE`]) when the value is not
    /// one of those spellings, worded like the framework's own refusal of a
    /// non-boolean `--quiet=maybe`.
    pub fn flag_on(&self, key: &str) -> Result<bool, CommandError> {
        self.flag(key).map_or(Ok(false), |value| {
            reserved_flags::parse_bool(value).ok_or_else(|| {
                reserved_flags::not_a_boolean(key, self.invocation.shown_flag_value(key, value))
            })
        })
    }

    // --- Standard agent-native flag accessors ---
    //
    // These read the conventional flag vocabulary so every agcli CLI speaks
    // the same dialect. The framework already parses the reserved booleans
    // anywhere on the line and refuses a value that is not a boolean
    // spelling (`--quiet=maybe` is `INVALID_FLAG`) before the handler runs;
    // these just give the handler a typed read. With the reserved flags
    // turned off nothing is refused, and only an "on" spelling reads as on.

    /// `--dry-run`: preview without mutating.
    #[must_use]
    pub fn dry_run(&self) -> bool {
        reserved_flags::bool_flag_on(self.flag("dry-run"))
    }

    /// `--quiet`: the caller wants no `next_actions` on success. The framework
    /// drops them itself; a handler reads this to skip work whose output the
    /// flag will drop, such as building those actions, or to stop narrating
    /// on stderr.
    #[must_use]
    pub fn quiet(&self) -> bool {
        reserved_flags::bool_flag_on(self.flag("quiet"))
    }

    /// `--yes` / `--no-input`: assume yes; never prompt interactively.
    #[must_use]
    pub fn assume_yes(&self) -> bool {
        reserved_flags::bool_flag_on(self.flag("yes"))
            || reserved_flags::bool_flag_on(self.flag("no-input"))
    }

    /// `--no-cache`: bypass any local cache.
    #[must_use]
    pub fn no_cache(&self) -> bool {
        reserved_flags::bool_flag_on(self.flag("no-cache"))
    }

    /// `--no-color`: emit machine-friendly, uncolored output.
    #[must_use]
    pub fn no_color(&self) -> bool {
        reserved_flags::bool_flag_on(self.flag("no-color"))
    }

    /// `--compact`: the caller wants only the fields the command declares
    /// with [`CommandOutput::compact_fields`](crate::CommandOutput::compact_fields),
    /// or else no nulls and empty containers. The framework projects the
    /// result itself; a handler reads this to skip work whose output the
    /// projection will drop.
    #[must_use]
    pub fn compact(&self) -> bool {
        reserved_flags::bool_flag_on(self.flag("compact"))
    }

    /// `--stdin`: caller intends to pipe input. Read it with
    /// `std::io::read_to_string(std::io::stdin())`.
    #[must_use]
    pub fn wants_stdin(&self) -> bool {
        reserved_flags::bool_flag_on(self.flag("stdin"))
    }

    /// `--select=a,b,c`: the requested field projection, split and trimmed
    /// exactly as the framework's own projection reads it. Empty when the
    /// flag is absent, given bare, or names no field; empty entries are
    /// dropped.
    pub fn select(&self) -> impl Iterator<Item = &str> {
        self.invocation
            .value_flag("select")
            .into_iter()
            .flat_map(select::parse_select_fields)
    }
}

/// True unless `value` is a non-finite `f64`/`f32`. String parsing accepts
/// `"inf"`/`"NaN"` for the float types, but `serde_json` serializes non-finite
/// floats as `null`, an envelope-corrupting value the typed helpers refuse
/// to produce.
fn parsed_value_is_finite<T: Any>(value: &T) -> bool {
    let any = value as &dyn Any;
    any.downcast_ref::<f64>()
        .map(|f| f.is_finite())
        .or_else(|| any.downcast_ref::<f32>().map(|f| f.is_finite()))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::agent_cli::AgentCli;
    use crate::command::Command;
    use crate::command_output::CommandOutput;
    use crate::envelope::Envelope;

    fn request(args: &[&str]) -> CommandRequest {
        let invocation = Invocation::parse(args.iter().copied(), |_| false).expect("should parse");
        CommandRequest::new(Arc::new(invocation), 0, Arc::new(|| Timestamp::UNIX_EPOCH))
    }

    #[tokio::test]
    async fn now_reads_the_runs_clock() {
        // A handler stamping its own fields reads the clock the envelope and
        // the stream events read, so a pinned clock makes those byte-stable
        // too.
        let cli = AgentCli::new("app", "x").clock(|| 1_740_000_000).command(
            Command::new("make", "make")
                .usage("app make")
                .handler(|req| async move {
                    Ok(CommandOutput::new(json!({ "created_at": req.now() })))
                }),
        );
        let run = cli.run(["app", "make"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope, got {}", run.to_json());
        };
        assert_eq!(envelope.result["created_at"], json!("2025-02-19T21:20:00Z"));
        assert_eq!(
            envelope.timestamp,
            envelope.result["created_at"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap()
        );
    }

    #[test]
    fn select_splits_like_the_framework_projection() {
        let select =
            |args: &[&str]| -> Vec<String> { request(args).select().map(str::to_string).collect() };
        assert_eq!(select(&["app", "--select= id, ,name,"]), ["id", "name"]);
        let none: [String; 0] = [];
        assert_eq!(select(&["app"]), none);
        assert_eq!(select(&["app", "--select="]), none);
        // Given bare, the flag names no field: not one called `true`.
        assert_eq!(select(&["app", "--select"]), none);
        assert_eq!(select(&["app", "--select=true"]), ["true"]);
    }

    // --- Strict boolean reads ---

    #[test]
    fn flag_on_is_off_when_the_flag_is_absent() {
        assert!(!request(&["app"]).flag_on("force").unwrap());
    }

    #[test]
    fn flag_on_reads_a_bare_flag_as_on() {
        assert!(request(&["app", "--force"]).flag_on("force").unwrap());
    }

    #[test]
    fn flag_on_reads_every_off_spelling_as_off() {
        for off in ["false", "0", "no", "off", "OFF", "False"] {
            let flag = format!("--force={off}");
            assert!(!request(&["app", &flag]).flag_on("force").unwrap(), "{off}");
        }
    }

    #[test]
    fn flag_on_refuses_a_value_that_is_not_a_boolean() {
        let error = request(&["app", "--force=maybe"])
            .flag_on("force")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::INVALID_FLAG);
        assert_eq!(error.exit_code, Some(ExitCode::USAGE));
        assert_eq!(error.message, r#"flag --force is not a boolean: "maybe""#);
        assert!(
            error.fix.contains("true/false, 1/0, yes/no, on/off"),
            "{}",
            error.fix
        );

        let error = request(&["app", "-f=maybe"]).flag_on("f").unwrap_err();
        assert_eq!(error.message, r#"flag -f is not a boolean: "maybe""#);
    }

    // --- flag_parse names the flag as it is typed ---

    #[test]
    fn flag_parse_names_a_one_letter_flag_with_one_dash() {
        let error = request(&["app", "-n=lots"])
            .flag_parse::<usize>("n")
            .unwrap_err();
        assert!(
            error.message.starts_with("flag -n is not valid"),
            "{}",
            error.message
        );
        assert_eq!(error.fix, "Pass a valid value for -n.");

        let error = request(&["app", "-n=inf"])
            .flag_parse::<f64>("n")
            .unwrap_err();
        assert!(
            error.message.starts_with("flag -n is not finite"),
            "{}",
            error.message
        );
        assert_eq!(
            error.fix,
            "Pass a finite number for -n. JSON cannot represent infinities or NaN."
        );

        let error = request(&["app", "--lines=lots"])
            .flag_parse::<usize>("lines")
            .unwrap_err();
        assert_eq!(error.fix, "Pass a valid value for --lines.");
    }

    #[test]
    fn flag_parse_refuses_a_bare_flag_as_given_no_value() {
        // A bare value flag reads as "true"; the error used to blame that
        // value, which nobody typed.
        let error = request(&["app", "--limit"])
            .flag_parse::<usize>("limit")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::INVALID_FLAG);
        assert_eq!(error.exit_code, Some(ExitCode::USAGE));
        assert_eq!(error.message, "flag --limit was given no value");
        assert_eq!(error.fix, "Pass a value: --limit=<value>");

        let error = request(&["app", "-n"])
            .flag_parse::<usize>("n")
            .unwrap_err();
        assert_eq!(error.message, "flag -n was given no value");
        assert_eq!(error.fix, "Pass a value: -n=<value>");

        // A typed value is still the one parsed, even when it spells "true".
        assert_eq!(
            request(&["app", "--name=true"])
                .flag_parse::<String>("name")
                .unwrap(),
            Some("true".to_string())
        );
        assert_eq!(
            request(&["app", "--limit", "5"])
                .flag_parse::<usize>("limit")
                .unwrap(),
            Some(5)
        );
    }

    // --- Standard flag accessors ---

    #[tokio::test]
    async fn standard_flag_accessors_read_convention_flags() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("run", "run").handles_dry_run().handler(|req| {
                let flags = json!({
                    "dry_run": req.dry_run(),
                    "assume_yes": req.assume_yes(),
                    "no_cache": req.no_cache(),
                    "wants_stdin": req.wants_stdin(),
                });
                async move { Ok(CommandOutput::new(flags)) }
            }),
        );
        let run = cli
            .run([
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

    // --- Typed argument helpers ---

    #[tokio::test]
    async fn arg_parse_helpers_fold_missing_and_invalid() {
        let cli = AgentCli::new("calc", "x").command(
            Command::new("add", "add")
                .usage("calc add <a> <b>")
                .handler(|req| {
                    let a = req.arg_parse::<f64>(0, "a");
                    let b = req.arg_parse::<f64>(1, "b");
                    async move {
                        let sum = a? + b?;
                        Ok(CommandOutput::new(json!({ "sum": sum })))
                    }
                }),
        );
        let run = cli.run(["calc", "add", "3", "5"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["sum"], json!(8.0));

        let run = cli.run(["calc", "add", "3"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, ErrorCode::MISSING_ARG);

        let run = cli.run(["calc", "add", "x", "5"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, ErrorCode::INVALID_ARG);
    }

    // --- Non-finite floats are rejected by the typed helpers (R8) ---

    #[tokio::test]
    async fn arg_parse_rejects_non_finite_floats() {
        let cli = AgentCli::new("calc", "calc").command(
            Command::new("add", "Add")
                .usage("calc add <a> <b>")
                .handler(|req| {
                    let a = req.arg_parse::<f64>(0, "a");
                    let b = req.arg_parse::<f64>(1, "b");
                    async move {
                        let sum = a? + b?;
                        Ok(CommandOutput::new(json!({ "sum": sum })))
                    }
                }),
        );
        for bad in ["inf", "-inf", "NaN", "infinity"] {
            let run = cli.run(["calc", "add", bad, "1"]).await;
            let Envelope::Error(envelope) = run.envelope() else {
                panic!("expected error for {bad:?}, got {}", run.to_json());
            };
            assert_eq!(envelope.error.code, ErrorCode::INVALID_ARG, "{bad:?}");
            assert!(envelope.error.message.contains("not finite"), "{bad:?}");
        }
    }
}
