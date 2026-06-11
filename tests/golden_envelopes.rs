//! Golden-envelope tests: pin the exact bytes the framework emits for the
//! canonical invocation classes. The envelope is hand-serialized
//! (`src/envelope.rs`), so a field reorder or rename is a breaking change
//! agents feel instantly — these tests turn schema drift into a red test
//! instead of a downstream agent outage, and double as a living spec.
//!
//! Timestamps are pinned via `SOURCE_DATE_EPOCH=0` (set per test process;
//! `cargo nextest` runs each test in its own process).

use agcli::{ActionParam, AgentCli, Command, CommandError, CommandOutput, ExitCode, NextAction};
use serde_json::json;

fn pin_clock() {
    // SAFETY: nextest runs each test in its own process; no other thread is
    // reading the environment concurrently at this point.
    unsafe { std::env::set_var("SOURCE_DATE_EPOCH", "0") };
}

/// The fixed calc-shaped CLI every golden case runs against.
fn golden_cli() -> AgentCli {
    AgentCli::new("calc", "Agent-native calculator")
        .version("1.0.0")
        .command(
            Command::new("add", "Add two numbers")
                .usage("calc add <a> <b>")
                .handler(|req, _ctx| {
                    let a = req.arg_parse::<f64>(0, "a");
                    let b = req.arg_parse::<f64>(1, "b");
                    Box::pin(async move {
                        let sum = a? + b?;
                        Ok(CommandOutput::new(json!({
                            "operation": "add",
                            "result": sum,
                        }))
                        .next_action(
                            NextAction::new("calc add <a> <b>", "Add two more numbers")
                                .with_param("a", ActionParam::new().value(json!(sum)))
                                .with_param("b", ActionParam::new().required(true)),
                        ))
                    })
                }),
        )
        .command(
            Command::new("fail", "Always fails with a typed error")
                .usage("calc fail")
                .handler(|_req, _ctx| {
                    Box::pin(async move {
                        Err::<CommandOutput, _>(
                            CommandError::new(
                                "the thing was not found",
                                "NOT_FOUND",
                                "Create the thing first with `calc add <a> <b>`.",
                            )
                            .exit_code(ExitCode::NOT_FOUND),
                        )
                    })
                }),
        )
}

async fn run(args: &[&str]) -> String {
    pin_clock();
    golden_cli().run_argv(args.iter().copied()).await.to_json()
}

/// Each case: (name, argv, expected exact JSON line).
/// Regenerate an entry by running the invocation under `SOURCE_DATE_EPOCH=0`
/// and reading stdout — but treat any diff as a schema break to justify.
macro_rules! golden {
    ($name:ident, $args:expr, $expected:expr) => {
        #[tokio::test]
        async fn $name() {
            let actual = run($args).await;
            assert_eq!(actual, $expected, "envelope bytes drifted");
        }
    };
}

golden!(
    success_envelope,
    &["calc", "add", "1", "2"],
    r#"{"ok":true,"command":"calc add 1 2","timestamp":"1970-01-01T00:00:00Z","exit_code":0,"result":{"operation":"add","result":3.0},"next_actions":[{"command":"calc add <a> <b>","description":"Add two more numbers","params":{"a":{"value":3.0},"b":{"required":true}}}]}"#
);

golden!(
    version_flag,
    &["calc", "--version"],
    r#"{"ok":true,"command":"calc --version","timestamp":"1970-01-01T00:00:00Z","exit_code":0,"result":{"name":"calc","version":"1.0.0"},"next_actions":[{"command":"calc","description":"Inspect the full command tree"}]}"#
);

golden!(
    version_positional_alias,
    &["calc", "version"],
    r#"{"ok":true,"command":"calc version","timestamp":"1970-01-01T00:00:00Z","exit_code":0,"result":{"name":"calc","version":"1.0.0"},"next_actions":[{"command":"calc","description":"Inspect the full command tree"}]}"#
);

golden!(
    help_command,
    &["calc", "help", "add"],
    r#"{"ok":true,"command":"calc help add","timestamp":"1970-01-01T00:00:00Z","exit_code":0,"result":{"description":"Add two numbers","name":"add","subcommands":[],"usage":"calc add <a> <b>"},"next_actions":[{"command":"calc add <a> <b>","description":"Run this command template","params":{"a":{"required":true},"b":{"required":true}}},{"command":"calc","description":"Inspect the full command tree"}]}"#
);

golden!(
    missing_arg_error,
    &["calc", "add", "1"],
    r#"{"ok":false,"command":"calc add 1","timestamp":"1970-01-01T00:00:00Z","exit_code":2,"error":{"message":"missing argument <b>","code":"MISSING_ARG","retryable":false},"fix":"Provide <b> as positional argument 1.","next_actions":[{"command":"calc add <a> <b>","description":"Run this command template","params":{"a":{"required":true},"b":{"required":true}}},{"command":"calc","description":"Inspect the full command tree"}]}"#
);

golden!(
    invalid_arg_error,
    &["calc", "add", "foo", "bar"],
    r#"{"ok":false,"command":"calc add foo bar","timestamp":"1970-01-01T00:00:00Z","exit_code":2,"error":{"message":"argument <a> is not valid: \"foo\"","code":"INVALID_ARG","retryable":false},"fix":"Pass a valid value for <a>.","next_actions":[{"command":"calc add <a> <b>","description":"Run this command template","params":{"a":{"required":true},"b":{"required":true}}},{"command":"calc","description":"Inspect the full command tree"}]}"#
);

golden!(
    unknown_command_error,
    &["calc", "plus", "1", "2"],
    r#"{"ok":false,"command":"calc plus 1 2","timestamp":"1970-01-01T00:00:00Z","exit_code":2,"error":{"message":"unknown command: plus","code":"UNKNOWN_COMMAND","retryable":false},"fix":"Valid commands: add, fail.","next_actions":[{"command":"calc add <a> <b>","description":"Add two numbers","params":{"a":{"required":true},"b":{"required":true}}},{"command":"calc fail","description":"Always fails with a typed error"}]}"#
);

golden!(
    extra_arg_error,
    &["calc", "add", "1", "2", "3"],
    r#"{"ok":false,"command":"calc add 1 2 3","timestamp":"1970-01-01T00:00:00Z","exit_code":2,"error":{"message":"unexpected extra argument(s): \"3\" (`add` takes 2 positional argument(s); got 3)","code":"EXTRA_ARG","retryable":false},"fix":"Nothing was run. Re-invoke matching the usage template `calc add <a> <b>`, or drop the extra argument(s).","next_actions":[{"command":"calc add <a> <b>","description":"Run this command template","params":{"a":{"required":true},"b":{"required":true}}},{"command":"calc","description":"Inspect the full command tree"}]}"#
);

golden!(
    unknown_flag_error,
    &["calc", "add", "1", "2", "--bogus"],
    r#"{"ok":false,"command":"calc add 1 2 --bogus","timestamp":"1970-01-01T00:00:00Z","exit_code":2,"error":{"message":"unknown flag(s): --bogus","code":"UNKNOWN_FLAG","retryable":false},"fix":"`add` takes no flags of its own. Reserved agent flags (--select, --compact, --quiet, --dry-run, --yes, --no-input, --no-cache, --no-color, --stdin, --json, --version) are accepted on every command.","next_actions":[{"command":"calc add <a> <b>","description":"Run this command template","params":{"a":{"required":true},"b":{"required":true}}},{"command":"calc","description":"Inspect the full command tree"}]}"#
);

golden!(
    select_projection,
    &["calc", "add", "1", "2", "--select=result"],
    r#"{"ok":true,"command":"calc add 1 2 --select=result","timestamp":"1970-01-01T00:00:00Z","exit_code":0,"result":{"result":3.0},"next_actions":[{"command":"calc add <a> <b>","description":"Add two more numbers","params":{"a":{"value":3.0},"b":{"required":true}}}]}"#
);

golden!(
    select_no_match_warns_instead_of_wiping,
    &["calc", "add", "1", "2", "--select=bogus"],
    r#"{"ok":true,"command":"calc add 1 2 --select=bogus","timestamp":"1970-01-01T00:00:00Z","exit_code":0,"result":{"operation":"add","result":3.0,"select_warning":"--select=bogus matched no fields. Available top-level fields: operation, result. Returning the full result; re-run --select with a valid field name."},"next_actions":[{"command":"calc add <a> <b>","description":"Add two more numbers","params":{"a":{"value":3.0},"b":{"required":true}}}]}"#
);

golden!(
    quiet_strips_next_actions,
    &["calc", "add", "1", "2", "--quiet"],
    r#"{"ok":true,"command":"calc add 1 2 --quiet","timestamp":"1970-01-01T00:00:00Z","exit_code":0,"result":{"operation":"add","result":3.0},"next_actions":[]}"#
);

golden!(
    dry_run_unsupported_error,
    &["calc", "add", "1", "2", "--dry-run"],
    r#"{"ok":false,"command":"calc add 1 2 --dry-run","timestamp":"1970-01-01T00:00:00Z","exit_code":2,"error":{"message":"`add` does not support --dry-run","code":"DRY_RUN_UNSUPPORTED","retryable":false},"fix":"Nothing was changed. This command has no preview mode: run it without --dry-run to execute it, or inspect current state first with a read command from next_actions.","next_actions":[{"command":"calc add <a> <b>","description":"Run this command template","params":{"a":{"required":true},"b":{"required":true}}},{"command":"calc","description":"Inspect the full command tree"}]}"#
);

golden!(
    handler_error_with_typed_exit,
    &["calc", "fail"],
    r#"{"ok":false,"command":"calc fail","timestamp":"1970-01-01T00:00:00Z","exit_code":3,"error":{"message":"the thing was not found","code":"NOT_FOUND","retryable":false},"fix":"Create the thing first with `calc add <a> <b>`.","next_actions":[{"command":"calc fail","description":"Run this command template"},{"command":"calc","description":"Inspect the full command tree"}]}"#
);

/// The root tree is large; rather than pinning the whole line, pin its
/// structural contract: key order-independent presence of every section plus
/// determinism across two builds.
#[tokio::test]
async fn root_tree_structure_and_determinism() {
    let first = run(&["calc"]).await;
    let second = run(&["calc"]).await;
    assert_eq!(first, second, "root tree must be byte-deterministic");
    let value: serde_json::Value = serde_json::from_str(&first).expect("valid JSON");
    for key in [
        "description",
        "version",
        "commands",
        "agent_flags",
        "exit_codes",
        "error_codes",
    ] {
        assert!(
            value["result"].get(key).is_some(),
            "root tree lost its `{key}` section"
        );
    }
}
