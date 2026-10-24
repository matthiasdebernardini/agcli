//! The framework-reserved flags: the agent flags (`--select`, `--quiet`,
//! `--dry-run`, …) and the framework's own `--help`/`-h` and
//! `--version`/`-V`.

use crate::codes::{ErrorCode, ExitCode};
use crate::command_error::CommandError;
use crate::usage;

/// Every agent flag the framework reserves (without the leading `--`), in the
/// order the root tree documents them. `select` is the one value flag
/// ([`RESERVED_VALUE_FLAG`]); the rest are pure booleans, parsed as such on
/// every command so an agent can pass them anywhere without the parser
/// mistaking the next token for their value.
const RESERVED_FLAG_NAMES: &[&str] = &[
    "select", "compact", "quiet", "dry-run", "yes", "no-input", "no-cache", "no-color", "stdin",
    "json",
];

/// The one reserved agent flag that takes a value.
const RESERVED_VALUE_FLAG: &str = "select";

/// The flags the framework answers itself on every non-raw command, whether
/// or not the reserved agent flags are on: `--help`/`-h` and
/// `--version`/`-V`. Booleans, so `-V status` never reads `status` as a value.
/// A command cannot own them; [`AgentCli::audit`](crate::AgentCli::audit)
/// reports a usage that declares one.
pub(crate) const FRAMEWORK_FLAG_NAMES: &[&str] = &["help", "h", "version", "V"];

/// True for any reserved agent flag, the value flag included.
pub(crate) fn is_reserved(flag: &str) -> bool {
    RESERVED_FLAG_NAMES.contains(&flag)
}

/// True for a reserved agent flag the parser reads as a boolean.
pub(crate) fn is_reserved_bool(flag: &str) -> bool {
    flag != RESERVED_VALUE_FLAG && is_reserved(flag)
}

/// True for one of the framework's own flags ([`FRAMEWORK_FLAG_NAMES`]).
pub(crate) fn is_framework_flag(flag: &str) -> bool {
    FRAMEWORK_FLAG_NAMES.contains(&flag)
}

/// The boolean a flag value spells: `true`/`1`/`yes`/`on` or
/// `false`/`0`/`no`/`off`, in any ASCII case. `None` for anything else.
pub(crate) fn parse_bool(value: &str) -> Option<bool> {
    const ON: [&str; 4] = ["true", "1", "yes", "on"];
    const OFF: [&str; 4] = ["false", "0", "no", "off"];
    if ON.iter().any(|on| value.eq_ignore_ascii_case(on)) {
        Some(true)
    } else if OFF.iter().any(|off| value.eq_ignore_ascii_case(off)) {
        Some(false)
    } else {
        None
    }
}

/// Read a boolean flag's effective state from its parsed value: on when the
/// flag is present with an "on" spelling (a bare `--quiet` parses as
/// `"true"`), off when absent or spelled off (`--dry-run=false`). Without the
/// off spellings `--dry-run=false` would *enable* dry-run, punishing the agent
/// that spelled its intent out.
///
/// Dispatch rejects any other value of a reserved or framework boolean with
/// `INVALID_FLAG` before anything reads it (see [`invalid_bool_flag`]), so
/// for those flags "not a boolean" never reaches here.
pub(crate) fn bool_flag_on(value: Option<&str>) -> bool {
    value.and_then(parse_bool) == Some(true)
}

/// The `INVALID_FLAG` error (exit code [`ExitCode::USAGE`]) for boolean flag
/// `name` given `value`, which is not a boolean spelling. One wording for the
/// framework's own booleans and for [`CommandRequest::flag_on`](crate::CommandRequest::flag_on).
pub(crate) fn not_a_boolean(name: &str, value: &str) -> CommandError {
    let flag = usage::flag_display(name);
    CommandError::new(
        ErrorCode::INVALID_FLAG,
        format!("flag {flag} is not a boolean: {value:?}"),
        format!(
            "Pass {flag} alone to turn it on, or give it one of true/false, 1/0, \
             yes/no, on/off."
        ),
    )
    .exit_code(ExitCode::USAGE)
}

/// The first boolean flag on `flags` (a framework flag, or a reserved agent
/// flag when `reserved` is on) whose value is not a boolean spelling, with
/// that value. Names are checked in sorted order, so the answer does not
/// depend on map iteration order.
pub(crate) fn invalid_bool_flag<'a>(
    flags: impl IntoIterator<Item = (&'a String, &'a String)>,
    reserved: bool,
) -> Option<(&'a str, &'a str)> {
    let mut invalid: Vec<(&str, &str)> = flags
        .into_iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .filter(|(name, value)| {
            (is_framework_flag(name) || (reserved && is_reserved_bool(name)))
                && parse_bool(value).is_none()
        })
        .collect();
    invalid.sort_unstable();
    invalid.into_iter().next()
}

/// The framework-reserved agent-native flags with their semantics, in the
/// order they appear in the self-documenting root tree. `--select` is a value
/// flag; the rest are booleans (see [`RESERVED_FLAG_NAMES`]).
pub(crate) const RESERVED_FLAG_DOCS: &[(&str, &str)] = &[
    (
        "--select=<a,b,c>",
        "Project the result to these top-level keys or `a.b` dot paths, mapped over arrays. Names that match nothing are reported in a top-level `select_warning`, and when none match, the full result is kept.",
    ),
    (
        "--compact",
        "Drop null values and empty containers from the result, or keep only the fields the command declares with `compact_fields`. Beside `--select` it only drops nulls and empty containers from the selection, and when that would empty the selection it returns the selection uncompacted with a `select_warning`.",
    ),
    (
        "--quiet",
        "Drop `next_actions` from a success envelope. An error envelope keeps them: they are the way out.",
    ),
    (
        "--dry-run",
        "Preview without changing anything. A command without a preview mode refuses it with `DRY_RUN_UNSUPPORTED` and runs nothing.",
    ),
    (
        "--yes / --no-input",
        "Assume yes and never prompt interactively.",
    ),
    ("--no-cache", "Bypass any local cache."),
    ("--no-color", "Emit uncolored output."),
    ("--stdin", "Read the command's input from stdin."),
    (
        "--json",
        "Accepted and ignored: output is always JSON. Reserved because `--json` is the flag an agent most often guesses on an unfamiliar tool.",
    ),
];

/// Every reserved agent flag name (without the leading `--`), for runtime
/// discovery. These names are reserved unless
/// [`AgentCli::without_reserved_flags`](crate::AgentCli::without_reserved_flags)
/// turned them off: the framework parses and acts on them on every command.
/// `select` is a value flag; the others are parsed as booleans anywhere on
/// the line. The framework's own `--help`/`-h` and `--version`/`-V` are not
/// listed: they are reserved always.
#[must_use]
pub fn reserved_flag_names() -> &'static [&'static str] {
    RESERVED_FLAG_NAMES
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::agent_cli::AgentCli;
    use crate::command::Command;
    use crate::command_output::CommandOutput;
    use crate::envelope::Envelope;
    use crate::test_support::{rich_cli, sample_cli};

    // --- Reserved output flags: --compact / --quiet ---

    #[tokio::test]
    async fn compact_drops_null_and_empty_fields() {
        let cli = rich_cli();
        let run = cli.run(["app", "get", "1", "--compact"]).await;
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
    async fn quiet_drops_next_actions() {
        let cli = rich_cli();
        let run = cli.run(["app", "get", "1", "--quiet"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(
            envelope.next_actions.is_empty(),
            "expected no next actions, got {:?}",
            envelope.next_actions
        );
    }

    #[tokio::test]
    async fn reserved_flags_can_be_disabled() {
        // With reserved flags off, --select is just an ordinary flag and the
        // result passes through untouched.
        let cli = rich_cli().without_reserved_flags();
        let run = cli.run(["app", "get", "1", "--select", "id"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.result.get("body").is_some());
    }

    #[tokio::test]
    async fn reserved_bool_flag_does_not_swallow_positional() {
        // `--dry-run` is a framework-reserved bool, so `get --dry-run 1` keeps
        // `1` as the positional id even though `--dry-run` precedes it.
        let cli = AgentCli::new("app", "x").command(
            Command::new("get", "get").handles_dry_run().handler(|req| {
                let id = req.arg(0).unwrap_or("none").to_string();
                let dry = req.dry_run();
                async move { Ok(CommandOutput::new(json!({ "id": id, "dry": dry }))) }
            }),
        );
        let run = cli.run(["app", "get", "--dry-run", "1"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result, json!({ "id": "1", "dry": true }));
    }

    #[test]
    fn reserved_flag_names_includes_select_and_bools() {
        let names = reserved_flag_names();
        assert!(names.contains(&"select"));
        assert!(names.contains(&"quiet"));
        assert!(names.contains(&"dry-run"));
    }

    // --- Explicit negation turns reserved booleans off (R7) ---

    #[tokio::test]
    async fn dry_run_false_does_not_trigger_dry_run_gate() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "status", "--dry-run=false"]).await;
        assert!(
            matches!(run.envelope(), Envelope::Success(_)),
            "{}",
            run.to_json()
        );
    }

    #[tokio::test]
    async fn quiet_false_keeps_next_actions() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "status", "--quiet=false"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success");
        };
        assert!(
            !envelope.next_actions.is_empty(),
            "expected next actions, got none"
        );
    }
}
