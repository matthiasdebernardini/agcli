//! Raw passthrough commands (`Command::raw_handler`).
//!
//! A raw command owns its argv, its stdout, and its exit code. These tests
//! pin the three halves of that contract: argv arrives verbatim, none of the
//! framework's parse-time gates fire, and the exit code the handler returns
//! survives — including `1` meaning "no matches", not "failure".
//!
//! The handler records what it received in the [`ExecutionContext`], which is
//! the observable side channel a test has: the real stdout goes to the
//! process, and `stdout_is_raw_end_to_end` covers that end of the contract by
//! running the shipped example.

use agcli::{AgentCli, Command, CommandOutput, ExecutionContext};
use serde_json::{Value, json};

/// A CLI with one raw command that reports its argv into the context and
/// returns `exit` as the process status.
fn raw_cli(exit: i32) -> AgentCli {
    AgentCli::new("app", "Test CLI").command(
        Command::new("grep", "Search the index (ripgrep-compatible output)")
            .usage("app grep [rg-flags...] <pattern> [path...]")
            .raw_handler(move |args, ctx| {
                let seen: Vec<Value> = args.iter().map(|a| json!(a)).collect();
                ctx.set("argv", Value::Array(seen));
                Box::pin(async move { exit })
            }),
    )
}

async fn run(cli: &AgentCli, argv: &[&str]) -> (agcli::Execution, ExecutionContext) {
    let mut ctx = ExecutionContext::default();
    let execution = cli.run_argv_with_context(argv.to_vec(), &mut ctx).await;
    (execution, ctx)
}

fn seen_argv(ctx: &ExecutionContext) -> Vec<String> {
    ctx.get("argv")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
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
    let (execution, ctx) = run(&raw_cli(0), &argv).await;

    assert_eq!(seen_argv(&ctx), &argv[2..]);
    assert_eq!(execution.exit_code(), 0);
    assert!(execution.is_raw());
}

#[tokio::test]
async fn exit_code_one_means_no_hits_not_failure() {
    let (execution, _) = run(&raw_cli(1), &["app", "grep", "nothing"]).await;

    assert_eq!(execution.exit_code(), 1);
    assert!(execution.is_raw());
    // Still a success envelope: the command ran and answered. The `1` is the
    // command's own vocabulary, not a framework failure.
    assert!(execution.envelope().ok());
}

#[tokio::test]
async fn unknown_flags_are_the_handlers_business() {
    // `--lmit` on a normal command is a hard UNKNOWN_FLAG error. A raw command
    // never sees flag validation at all.
    let (execution, ctx) = run(&raw_cli(0), &["app", "grep", "--lmit", "3", "pat"]).await;

    assert!(execution.is_raw());
    assert_eq!(seen_argv(&ctx), ["--lmit", "3", "pat"]);
}

#[tokio::test]
async fn extra_positionals_are_not_rejected() {
    let (execution, ctx) = run(&raw_cli(0), &["app", "grep", "a", "b", "c", "d"]).await;

    assert!(execution.is_raw());
    assert_eq!(seen_argv(&ctx), ["a", "b", "c", "d"]);
}

#[tokio::test]
async fn dry_run_is_passed_through_not_gated() {
    // A normal command without `handles_dry_run()` refuses with
    // DRY_RUN_UNSUPPORTED. A raw command receives the token like any other.
    let (execution, ctx) = run(&raw_cli(0), &["app", "grep", "--dry-run", "pat"]).await;

    assert!(execution.is_raw());
    assert_eq!(seen_argv(&ctx), ["--dry-run", "pat"]);
}

#[tokio::test]
async fn help_flag_belongs_to_the_command() {
    let (execution, ctx) = run(&raw_cli(0), &["app", "grep", "--help"]).await;

    assert!(execution.is_raw());
    assert_eq!(seen_argv(&ctx), ["--help"]);
}

#[tokio::test]
async fn framework_help_is_still_reachable_by_name() {
    // `app grep --help` goes to the handler, so `app help grep` is how an
    // agent asks the framework instead.
    let (execution, _) = run(&raw_cli(0), &["app", "help", "grep"]).await;

    assert!(!execution.is_raw());
    let json: Value = serde_json::from_str(&execution.to_json()).unwrap();
    assert_eq!(json["result"]["name"], json!("grep"));
}

#[tokio::test]
async fn a_help_flag_behind_a_global_flag_still_belongs_to_the_command() {
    // The recovery path used to sit after the `--help` interception, so this
    // shape answered with a help envelope and never ran the handler.
    let (execution, ctx) = run(&raw_cli(0), &["app", "--json", "grep", "--help"]).await;

    assert!(execution.is_raw());
    assert_eq!(seen_argv(&ctx), ["--help"]);
}

#[tokio::test]
async fn a_short_help_flag_behind_a_global_flag_belongs_to_the_command() {
    let (execution, ctx) = run(&raw_cli(0), &["app", "--quiet", "grep", "-h", "pat"]).await;

    assert!(execution.is_raw());
    assert_eq!(seen_argv(&ctx), ["-h", "pat"]);
}

#[tokio::test]
async fn a_line_the_parser_rejects_still_reaches_the_handler() {
    // `--=x` is a parse error. Judging a raw command's syntax is exactly what
    // the framework promised not to do, so the handler gets it anyway.
    let (execution, ctx) = run(&raw_cli(0), &["app", "--json", "grep", "--=x"]).await;

    assert!(execution.is_raw());
    assert_eq!(seen_argv(&ctx), ["--=x"]);
}

#[tokio::test]
async fn a_value_flag_may_repeat_the_command_name() {
    // `--select` eats the first `grep` as its value; the second is the
    // command. Splitting argv by searching for the command name would hand the
    // handler ["grep", "pat"] — the parser's positional indices get it right.
    let (execution, ctx) = run(&raw_cli(0), &["app", "--select", "grep", "grep", "pat"]).await;

    assert!(execution.is_raw());
    assert_eq!(seen_argv(&ctx), ["pat"]);
}

#[tokio::test]
async fn an_undeclared_flag_may_repeat_the_command_name() {
    let (execution, ctx) = run(
        &raw_cli(0),
        &["app", "--unknownflag", "grep", "grep", "pat"],
    )
    .await;

    assert!(execution.is_raw());
    assert_eq!(seen_argv(&ctx), ["pat"]);
}

#[tokio::test]
async fn a_nested_raw_command_behind_a_global_flag_splits_correctly() {
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("code", "Code tools")
            .usage("app code <subcommand>")
            .subcommand(
                Command::new("grep", "Search")
                    .usage("app code grep [args...]")
                    .raw_handler(|args, ctx| {
                        let seen: Vec<Value> = args.iter().map(|a| json!(a)).collect();
                        ctx.set("argv", Value::Array(seen));
                        Box::pin(async move { 0 })
                    }),
            ),
    );
    let (execution, ctx) = run(&cli, &["app", "--json", "code", "grep", "-i", "pat"]).await;

    assert!(execution.is_raw());
    assert_eq!(seen_argv(&ctx), ["-i", "pat"]);
}

#[tokio::test]
async fn a_forwarded_signal_status_is_truncated_not_panicked() {
    // `Command::status().code()` gives -1 for "killed by a signal". The OS
    // truncates that to 255; so does the envelope, so both agree.
    let (execution, _) = run(&raw_cli(-1), &["app", "grep", "pat"]).await;

    assert_eq!(execution.exit_code(), 255);
    assert!(execution.is_raw());
}

#[tokio::test]
async fn reached_through_a_leading_global_flag() {
    // The lexical Pass-0 scan misses when a flag precedes the command name;
    // the normal path must still hand over, never emit an envelope.
    let (execution, ctx) = run(&raw_cli(0), &["app", "--json", "grep", "pat"]).await;

    assert!(execution.is_raw());
    assert_eq!(seen_argv(&ctx), ["pat"]);
}

#[tokio::test]
async fn a_panicking_raw_handler_never_lands_on_stdout() {
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("boom", "Panics")
            .usage("app boom")
            .raw_handler(|_args, _ctx| Box::pin(async move { panic!("kaboom") })),
    );
    let (execution, _) = run(&cli, &["app", "boom"]).await;

    assert!(execution.is_raw());
    assert_eq!(execution.exit_code(), 1);
    let json: Value = serde_json::from_str(&execution.to_json()).unwrap();
    assert_eq!(json["error"]["code"], json!("HANDLER_PANIC"));
}

#[tokio::test]
async fn raw_commands_are_in_the_tree_and_marked() {
    let (execution, _) = run(&raw_cli(0), &["app"]).await;
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
            .handler(|_req, _ctx| Box::pin(async move { Ok(CommandOutput::new(json!({}))) })),
    );
    let (execution, _) = run(&cli, &["app"]).await;
    let json: Value = serde_json::from_str(&execution.to_json()).unwrap();

    assert!(json["result"]["commands"][0].get("raw").is_none());
}

#[test]
fn audit_accepts_a_raw_command() {
    let report = raw_cli(0).audit();
    assert!(report.is_clean(), "{:?}", report.findings);
    assert_eq!(report.warning_count(), 0, "{:?}", report.findings);
}

#[test]
fn audit_flags_unreachable_subcommands_under_a_raw_command() {
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("grep", "Search")
            .usage("app grep [args...]")
            .raw_handler(|_args, _ctx| Box::pin(async move { 0 }))
            .subcommand(
                Command::new("index", "Index")
                    .usage("app grep index")
                    .handler(|_req, _ctx| {
                        Box::pin(async move { Ok(CommandOutput::new(json!({}))) })
                    }),
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

#[test]
fn audit_flags_a_command_carrying_both_handlers() {
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("grep", "Search")
            .usage("app grep [args...]")
            .handler(|_req, _ctx| Box::pin(async move { Ok(CommandOutput::new(json!({}))) }))
            .raw_handler(|_args, _ctx| Box::pin(async move { 0 })),
    );
    let report = cli.audit();

    assert!(!report.is_clean());
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.code == "RAW_COMMAND_HAS_HANDLER"),
        "{:?}",
        report.findings
    );
}

#[test]
fn audit_warns_when_a_raw_command_has_no_usage() {
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("grep", "Search").raw_handler(|_args, _ctx| Box::pin(async move { 0 })),
    );
    let report = cli.audit();

    assert!(report.is_clean());
    assert!(
        report.findings.iter().any(|f| f.code == "MISSING_USAGE"),
        "{:?}",
        report.findings
    );
}

/// Run the shipped `ops` example with the given arguments.
///
/// The nested cargo gets its own `CARGO_TARGET_DIR`, so it takes its own
/// build-directory lock instead of queueing behind the outer test run.
///
/// (A machine that also points `build.build-dir` at a cache shared across
/// projects still serializes this build against every other project's — that
/// is the shared cache working as configured, not something a test can route
/// around.)
fn run_ops_example(args: &[&str]) -> std::process::Output {
    let target_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("raw-passthrough-e2e");
    let mut command = std::process::Command::new(env!("CARGO"));
    command
        .args(["run", "-q", "--example", "ops", "--"])
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("CARGO_TARGET_DIR", target_dir);
    command.output().expect("run the ops example")
}

/// The half no in-process assertion can reach: that stdout carries the
/// command's own bytes and nothing else. The example's `echo` command is a
/// raw passthrough.
#[test]
fn stdout_is_raw_end_to_end() {
    let output = run_ops_example(&["echo", "-C", "3", "--not-a-flag", "hello"]);

    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "-C\n3\n--not-a-flag\nhello\n"
    );
    assert_eq!(output.status.code(), Some(0));

    // No arguments echoed: the example returns grep's "nothing matched" code.
    let empty = run_ops_example(&["echo"]);

    assert!(empty.stdout.is_empty());
    assert_eq!(empty.status.code(), Some(1));

    // The shapes that used to fall through to an envelope: a global flag
    // before the command name, `--help`, and a line the parser rejects.
    for args in [
        vec!["--json", "echo", "--help"],
        vec!["--quiet", "echo", "-h"],
        vec!["--json", "echo", "--=x"],
    ] {
        let out = run_ops_example(&args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("\"ok\""),
            "envelope leaked onto raw stdout for {args:?}: {stdout}"
        );
        assert_eq!(stdout, format!("{}\n", args[args.len() - 1]), "{args:?}");
    }
}
