//! Raw passthrough commands (`Command::raw_handler`).
//!
//! A raw command owns its argv, its stdout, and its exit code. These tests
//! pin the three halves of that contract: argv arrives verbatim, none of the
//! framework's parse-time gates fire, and the exit code the handler returns
//! survives, including `1` meaning "no matches", not "failure".
//!
//! The handler records what it received in a shared capture, which is the
//! observable side channel a test has: the real stdout goes to the process,
//! and `stdout_is_raw_end_to_end` covers that end of the contract by running
//! a raw command in a re-executed copy of this test binary. A panicking raw handler is covered in
//! `tests/panic_hook.rs`, in a subprocess.

use std::env;
use std::future::{self, Ready};
use std::io::{self, Write};
use std::iter;
use std::process::{self, Output};
use std::sync::{Arc, Mutex};

use agcli::{AgentCli, Command, CommandOutput, ExitCode};
use serde_json::{Value, json};
use tokio::runtime;

/// The argv a raw handler received.
type Seen = Arc<Mutex<Vec<String>>>;

/// A CLI and the capture its raw handler records into.
struct Fixture {
    cli: AgentCli,
    seen: Seen,
}

/// A raw handler that records its argv into `seen` and returns `exit`.
fn recording(seen: &Seen, exit: ExitCode) -> impl Fn(Vec<String>) -> Ready<ExitCode> + use<> {
    let seen = Arc::clone(seen);
    move |args| {
        *seen.lock().unwrap() = args;
        future::ready(exit)
    }
}

/// A CLI with one raw command that records its argv and returns `exit` as the
/// process status.
fn raw_cli(exit: ExitCode) -> Fixture {
    let seen = Seen::default();
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("grep", "Search the index (ripgrep-compatible output)")
            .usage("app grep [rg-flags...] <pattern> [path...]")
            .raw_handler(recording(&seen, exit)),
    );
    Fixture { cli, seen }
}

/// Run `argv` and return the execution plus the argv the handler saw.
async fn run(fixture: &Fixture, argv: &[&str]) -> (agcli::Execution, Vec<String>) {
    let execution = fixture.cli.run(argv.to_vec()).await;
    let seen = fixture.seen.lock().unwrap().clone();
    (execution, seen)
}

#[tokio::test]
async fn argv_arrives_verbatim() {
    // Every shape that a parser would otherwise eat: a value flag and its
    // value as separate tokens, a glob, a `--` separator, and a pattern that
    // starts with a dash.
    let argv = [
        "app",
        "grep",
        "-i",
        "-C",
        "3",
        "-t",
        "rust",
        "-g",
        "*.rs",
        "--",
        "-dashed-pattern",
        "src/",
    ];
    let (execution, seen) = run(&raw_cli(ExitCode::SUCCESS), &argv).await;

    assert_eq!(seen, &argv[2..]);
    assert_eq!(execution.exit_code(), ExitCode::SUCCESS);
    assert!(execution.is_raw());
}

#[tokio::test]
async fn exit_code_one_means_no_hits_not_failure() {
    let (execution, _) = run(&raw_cli(ExitCode::ERROR), &["app", "grep", "nothing"]).await;

    assert_eq!(execution.exit_code(), ExitCode::ERROR);
    assert!(execution.is_raw());
    // Still a success envelope: the command ran and answered. The `1` is the
    // command's own vocabulary, not a framework failure.
    assert!(execution.envelope().ok());
}

#[tokio::test]
async fn unknown_flags_are_the_handlers_business() {
    // `--lmit` on a normal command is a hard UNKNOWN_FLAG error. A raw command
    // never sees flag validation at all.
    let (execution, seen) = run(
        &raw_cli(ExitCode::SUCCESS),
        &["app", "grep", "--lmit", "3", "pat"],
    )
    .await;

    assert!(execution.is_raw());
    assert_eq!(seen, ["--lmit", "3", "pat"]);
}

#[tokio::test]
async fn extra_positionals_are_not_rejected() {
    let (execution, seen) = run(
        &raw_cli(ExitCode::SUCCESS),
        &["app", "grep", "a", "b", "c", "d"],
    )
    .await;

    assert!(execution.is_raw());
    assert_eq!(seen, ["a", "b", "c", "d"]);
}

#[tokio::test]
async fn dry_run_is_passed_through_not_gated() {
    // A normal command without `handles_dry_run()` refuses with
    // DRY_RUN_UNSUPPORTED. A raw command receives the token like any other.
    let (execution, seen) = run(
        &raw_cli(ExitCode::SUCCESS),
        &["app", "grep", "--dry-run", "pat"],
    )
    .await;

    assert!(execution.is_raw());
    assert_eq!(seen, ["--dry-run", "pat"]);
}

#[tokio::test]
async fn help_flag_belongs_to_the_command() {
    let (execution, seen) = run(&raw_cli(ExitCode::SUCCESS), &["app", "grep", "--help"]).await;

    assert!(execution.is_raw());
    assert_eq!(seen, ["--help"]);
}

#[tokio::test]
async fn version_flags_belong_to_the_command() {
    for flag in ["--version", "-V"] {
        let (execution, seen) = run(&raw_cli(ExitCode::SUCCESS), &["app", "grep", flag]).await;

        assert!(execution.is_raw(), "{flag}");
        assert_eq!(seen, [flag]);
    }
}

#[tokio::test]
async fn framework_help_is_still_reachable_by_name() {
    // `app grep --help` goes to the handler, so `app help grep` is how an
    // agent asks the framework instead.
    let (execution, _) = run(&raw_cli(ExitCode::SUCCESS), &["app", "help", "grep"]).await;

    assert!(!execution.is_raw());
    let json: Value = serde_json::from_str(&execution.to_json()).unwrap();
    assert_eq!(json["result"]["name"], json!("grep"));
}

#[tokio::test]
async fn a_help_flag_behind_a_global_flag_still_belongs_to_the_command() {
    // The recovery path used to sit after the `--help` interception, so this
    // shape answered with a help envelope and never ran the handler.
    let (execution, seen) = run(
        &raw_cli(ExitCode::SUCCESS),
        &["app", "--json", "grep", "--help"],
    )
    .await;

    assert!(execution.is_raw());
    assert_eq!(seen, ["--help"]);
}

#[tokio::test]
async fn a_short_help_flag_behind_a_global_flag_belongs_to_the_command() {
    let (execution, seen) = run(
        &raw_cli(ExitCode::SUCCESS),
        &["app", "--quiet", "grep", "-h", "pat"],
    )
    .await;

    assert!(execution.is_raw());
    assert_eq!(seen, ["-h", "pat"]);
}

#[tokio::test]
async fn a_help_flag_before_the_name_is_the_frameworks() {
    // `app --help grep` used to run grep with no arguments, and `app -h grep
    // pat` ran it with `pat`: the tokens before the name are the framework's.
    for argv in [
        &["app", "--help", "grep"][..],
        &["app", "-h", "grep", "pat"],
    ] {
        let fixture = raw_cli(ExitCode::SUCCESS);
        let (execution, seen) = run(&fixture, argv).await;

        assert!(!execution.is_raw(), "{argv:?}");
        assert!(seen.is_empty(), "the handler ran: {argv:?}");
        let json: Value = serde_json::from_str(&execution.to_json()).unwrap();
        assert_eq!(json["ok"], json!(true), "{argv:?}: {json}");
        assert_eq!(json["result"]["name"], json!("grep"), "{argv:?}: {json}");
        // The envelope names the whole line, not only the framework's part.
        assert_eq!(json["command"], json!(argv.join(" ")), "{argv:?}");
    }

    // After the name, the same flags are the command's.
    let (execution, seen) = run(
        &raw_cli(ExitCode::SUCCESS),
        &["app", "--json", "grep", "-h", "pat"],
    )
    .await;
    assert!(execution.is_raw());
    assert_eq!(seen, ["-h", "pat"]);
}

#[tokio::test]
async fn a_version_flag_before_the_name_is_the_frameworks() {
    let (execution, seen) = run(&raw_cli(ExitCode::SUCCESS), &["app", "-V", "grep", "pat"]).await;

    assert!(!execution.is_raw());
    assert!(seen.is_empty(), "the handler ran");
    let json: Value = serde_json::from_str(&execution.to_json()).unwrap();
    assert_eq!(json["result"], json!({ "name": "app" }), "{json}");
}

#[tokio::test]
async fn a_non_boolean_before_the_name_is_refused_like_anywhere_else() {
    // `--quiet=maybe` is INVALID_FLAG on every other line; before a raw
    // command's name it used to be dropped and the command run.
    let (execution, seen) = run(
        &raw_cli(ExitCode::SUCCESS),
        &["app", "--quiet=maybe", "grep", "pat"],
    )
    .await;

    assert!(!execution.is_raw());
    assert!(seen.is_empty(), "the handler ran");
    assert_eq!(execution.exit_code(), ExitCode::USAGE);
    let json: Value = serde_json::from_str(&execution.to_json()).unwrap();
    assert_eq!(json["error"]["code"], json!("INVALID_FLAG"), "{json}");
    assert_eq!(json["command"], json!("app --quiet=maybe grep pat"));
    assert_eq!(
        json["next_actions"][0]["command"],
        json!("app grep [rg-flags...] <pattern> [path...]"),
        "the raw command's own template: {json}"
    );
}

#[tokio::test]
async fn a_malformed_token_before_the_name_is_the_frameworks_parse_error() {
    // The command owns a malformed token after its name
    // (`a_line_the_parser_rejects_still_reaches_the_handler`); before it,
    // the token is the framework's, and the framework refuses it.
    let (execution, seen) = run(&raw_cli(ExitCode::SUCCESS), &["app", "--=x", "grep", "pat"]).await;

    assert!(!execution.is_raw());
    assert!(seen.is_empty(), "the handler ran");
    let json: Value = serde_json::from_str(&execution.to_json()).unwrap();
    assert_eq!(json["error"]["code"], json!("PARSE_ERROR"), "{json}");
    assert_eq!(json["error"]["message"], json!("invalid flag syntax: --=x"));
}

#[tokio::test]
async fn a_line_the_parser_rejects_still_reaches_the_handler() {
    // `--=x` is a parse error. Judging a raw command's syntax is exactly what
    // the framework promised not to do, so the handler gets it anyway.
    let (execution, seen) = run(
        &raw_cli(ExitCode::SUCCESS),
        &["app", "--json", "grep", "--=x"],
    )
    .await;

    assert!(execution.is_raw());
    assert_eq!(seen, ["--=x"]);
}

#[tokio::test]
async fn a_value_flag_may_repeat_the_command_name() {
    // `--select` eats the first `grep` as its value; the second is the
    // command. Splitting argv by searching for the command name would hand the
    // handler ["grep", "pat"]; the parser's positional indices get it right.
    let (execution, seen) = run(
        &raw_cli(ExitCode::SUCCESS),
        &["app", "--select", "grep", "grep", "pat"],
    )
    .await;

    assert!(execution.is_raw());
    assert_eq!(seen, ["pat"]);
}

#[tokio::test]
async fn an_undeclared_flag_may_repeat_the_command_name() {
    let (execution, seen) = run(
        &raw_cli(ExitCode::SUCCESS),
        &["app", "--unknownflag", "grep", "grep", "pat"],
    )
    .await;

    assert!(execution.is_raw());
    assert_eq!(seen, ["pat"]);
}

#[tokio::test]
async fn a_nested_raw_command_behind_a_global_flag_splits_correctly() {
    let seen = Seen::default();
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("code", "Code tools")
            .usage("app code <subcommand>")
            .subcommand(
                Command::new("grep", "Search")
                    .usage("app code grep [args...]")
                    .raw_handler(recording(&seen, ExitCode::SUCCESS)),
            ),
    );
    let (execution, seen) = run(
        &Fixture { cli, seen },
        &["app", "--json", "code", "grep", "-i", "pat"],
    )
    .await;

    assert!(execution.is_raw());
    assert_eq!(seen, ["-i", "pat"]);
}

#[tokio::test]
async fn the_highest_status_byte_survives() {
    // Every `ExitCode` is a status the process can exit with, so the top of
    // the range comes back unchanged.
    let (execution, _) = run(&raw_cli(ExitCode::new(255)), &["app", "grep", "pat"]).await;

    assert_eq!(execution.exit_code(), ExitCode::new(255));
    assert!(execution.is_raw());
}

#[tokio::test]
async fn reached_through_a_leading_global_flag() {
    // The lexical Pass-0 scan misses when a flag precedes the command name;
    // the normal path must still hand over, never emit an envelope.
    let (execution, seen) = run(
        &raw_cli(ExitCode::SUCCESS),
        &["app", "--json", "grep", "pat"],
    )
    .await;

    assert!(execution.is_raw());
    assert_eq!(seen, ["pat"]);
}

#[tokio::test]
async fn raw_commands_are_in_the_tree_and_marked() {
    let (execution, _) = run(&raw_cli(ExitCode::SUCCESS), &["app"]).await;
    let json: Value = serde_json::from_str(&execution.to_json()).unwrap();
    let grep = &json["result"]["commands"][0];

    assert_eq!(grep["name"], json!("grep"));
    assert_eq!(grep["raw"], json!(true));
    assert_eq!(
        grep["usage"],
        json!("app grep [rg-flags...] <pattern> [path...]")
    );
}

#[tokio::test]
async fn normal_commands_carry_no_raw_marker() {
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("status", "Status")
            .usage("app status")
            .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
    );
    let execution = cli.run(["app"]).await;
    let json: Value = serde_json::from_str(&execution.to_json()).unwrap();

    assert!(json["result"]["commands"][0].get("raw").is_none());
}

#[test]
fn audit_accepts_a_raw_command() {
    let report = raw_cli(ExitCode::SUCCESS).cli.audit();
    assert!(report.is_clean(), "{:?}", report.findings);
    assert_eq!(report.warning_count(), 0, "{:?}", report.findings);
}

#[test]
fn audit_flags_unreachable_subcommands_under_a_raw_command() {
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("grep", "Search")
            .usage("app grep [args...]")
            .raw_handler(|_args| async move { ExitCode::SUCCESS })
            .subcommand(
                Command::new("index", "Index")
                    .usage("app grep index")
                    .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
            ),
    );
    let report = cli.audit();

    assert!(!report.is_clean());
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.code == "RAW_COMMAND_HAS_SUBCOMMANDS"),
        "{:?}",
        report.findings
    );
}

#[tokio::test]
async fn a_later_handler_call_replaces_an_earlier_one() {
    let raw_last = AgentCli::new("app", "Test CLI").command(
        Command::new("grep", "Search")
            .usage("app grep [args...]")
            .handler(|_req| async move { Ok(CommandOutput::new(json!({ "envelope": true }))) })
            .raw_handler(|_args| async move { ExitCode::new(7) }),
    );
    let execution = raw_last.run(["app", "grep"]).await;
    assert!(execution.is_raw());
    assert_eq!(execution.exit_code(), ExitCode::new(7));

    let envelope_last = AgentCli::new("app", "Test CLI").command(
        Command::new("grep", "Search")
            .usage("app grep [args...]")
            .raw_handler(|_args| async move { ExitCode::new(7) })
            .handler(|_req| async move { Ok(CommandOutput::new(json!({ "envelope": true }))) }),
    );
    let execution = envelope_last.run(["app", "grep"]).await;
    assert!(!execution.is_raw());
    let json: Value = serde_json::from_str(&execution.to_json()).unwrap();
    assert_eq!(json["result"]["envelope"], json!(true), "{json}");
}

#[test]
fn audit_warns_when_a_raw_command_has_no_usage() {
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("grep", "Search").raw_handler(|_args| async move { ExitCode::SUCCESS }),
    );
    let report = cli.audit();

    assert!(report.is_clean());
    assert!(
        report.findings.iter().any(|f| f.code == "MISSING_USAGE"),
        "{:?}",
        report.findings
    );
}

/// Names, in the re-executed child, the argv its CLI runs after the program
/// name: the arguments joined with `ARGV_SEPARATOR`.
const CHILD_ARGV: &str = "AGCLI_TEST_CHILD_ARGV";

/// Separates the arguments in `CHILD_ARGV` (ASCII unit separator: no test
/// argument contains it).
const ARGV_SEPARATOR: char = '\u{1f}';

/// The line the child writes to stderr as it starts, so the parent knows the
/// child ran exactly once rather than passing on an empty stdout.
const CHILD_RAN: &str = "AGCLI_TEST_CHILD_RAN raw_commands::raw_echo_child";

/// The libtest harness's own first output on the child's stdout, before the
/// child test runs and ends the process.
const HARNESS_PREAMBLE: &str = "\nrunning 1 test\n";

/// The subprocess entry point for `stdout_is_raw_end_to_end`: a CLI whose
/// `echo` is a raw passthrough printing each argument on its own line and
/// returning grep's `1` when there is nothing to print. A no-op in a normal
/// test run.
#[test]
fn raw_echo_child() {
    let Ok(argv) = env::var(CHILD_ARGV) else {
        return;
    };
    // The child's stdout is the command's own, so the sentinel goes to
    // stderr, through the handle rather than `eprintln!`.
    writeln!(io::stderr(), "{CHILD_RAN}").expect("write the sentinel");
    let cli = AgentCli::new("app", "Raw end-to-end test CLI").command(
        Command::new("echo", "Print each argument on its own line")
            .usage("app echo [args...]")
            .raw_handler(|args| async move {
                for arg in &args {
                    println!("{arg}");
                }
                ExitCode::new(u8::from(args.is_empty()))
            }),
    );
    let argv: Vec<&str> = iter::once("app")
        .chain(argv.split(ARGV_SEPARATOR))
        .collect();
    let runtime = runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    runtime.block_on(cli.run(argv)).finish()
}

/// Re-run this test binary as the `raw_echo_child` child, its CLI on
/// `app <args>`.
///
/// The child is selected with `--exact`, so no other test can run in it
/// whatever names are added later. Its CLI reads its argv from `CHILD_ARGV`
/// rather than from the process argv, because libtest parses the process
/// argv and refuses the shapes this test needs (`-C`, `--not-a-flag`,
/// `--help`, `--=x`). `--nocapture` lets the handler's `println!` reach the
/// real stdout, and with more than one test thread libtest prints no test
/// name before the test runs, so stdout is `HARNESS_PREAMBLE` and then exactly
/// what the command printed: `finish` exits before libtest writes anything
/// else.
fn run_app(args: &[&str]) -> Output {
    let mut joined = String::new();
    for (index, arg) in args.iter().enumerate() {
        if index > 0 {
            joined.push(ARGV_SEPARATOR);
        }
        joined.push_str(arg);
    }
    let output = process::Command::new(env::current_exe().expect("current exe"))
        .args([
            "--exact",
            "raw_echo_child",
            "--nocapture",
            "--test-threads=2",
        ])
        .env(CHILD_ARGV, joined)
        .output()
        .expect("spawn the child");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches(CHILD_RAN).count(),
        1,
        "the child did not run exactly once: {stderr}"
    );
    output
}

/// The bytes the command itself wrote on the child's stdout.
fn command_stdout(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .strip_prefix(HARNESS_PREAMBLE)
        .unwrap_or_else(|| panic!("the harness preamble leads stdout: {stdout:?}"))
        .to_string()
}

/// The half no in-process assertion can reach: that stdout carries the
/// command's own bytes and nothing else, and its exit code is the process
/// status.
#[test]
fn stdout_is_raw_end_to_end() {
    let output = run_app(&["echo", "-C", "3", "--not-a-flag", "hello"]);

    assert_eq!(command_stdout(&output), "-C\n3\n--not-a-flag\nhello\n");
    assert_eq!(output.status.code(), Some(0));

    // No arguments echoed: the command returns grep's "nothing matched" code.
    let empty = run_app(&["echo"]);

    assert!(
        command_stdout(&empty).is_empty(),
        "expected no stdout, got {:?}",
        String::from_utf8_lossy(&empty.stdout)
    );
    assert_eq!(empty.status.code(), Some(1));

    // The shapes that used to fall through to an envelope: a global flag
    // before the command name, `--help`, and a line the parser rejects.
    for args in [
        vec!["--json", "echo", "--help"],
        vec!["--quiet", "echo", "-h"],
        vec!["--json", "echo", "--=x"],
    ] {
        let out = run_app(&args);
        let stdout = command_stdout(&out);
        assert!(
            !stdout.contains("\"ok\""),
            "envelope leaked onto raw stdout for {args:?}: {stdout}"
        );
        assert_eq!(stdout, format!("{}\n", args[args.len() - 1]), "{args:?}");
    }
}
