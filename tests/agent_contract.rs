//! Contract guarantees a migrating CLI leans on.
//!
//! Each test here pins a promise the crate makes but that no single API
//! signature states: what happens to a flag nobody declared, what the exit
//! code is when a command succeeds while the world is degraded, what a
//! skipped health check does to a `doctor` report, and whether a handler may
//! talk on stderr.

use agcli::{AgentCli, Check, CheckResult, Command, CommandOutput, ExitCode};
use serde_json::{Value, json};

fn parse(execution: &agcli::Execution) -> Value {
    serde_json::from_str(&execution.to_json()).expect("envelope is JSON")
}

/// A CLI whose `brain` command echoes what it saw, and whose usage declares
/// one optional positional and no `--json` flag.
fn cli() -> AgentCli {
    AgentCli::new("app", "Test CLI").command(
        Command::new("brain", "Report state")
            .usage("app brain [<repo>]")
            .handler(|req, _ctx| {
                let positionals = req.positionals().to_vec();
                let json_flag = req.flag("json").map(str::to_string);
                let quiet = req.quiet();
                Box::pin(async move {
                    // A handler may narrate on stderr; stdout belongs to the
                    // envelope alone. `--quiet` is the caller's signal to stop.
                    if !quiet {
                        eprintln!("brain: contacting the viewer…");
                    }
                    Ok(CommandOutput::new(json!({
                        "positionals": positionals,
                        "json_flag": json_flag,
                        "quiet": quiet,
                    })))
                })
            }),
    )
}

#[tokio::test]
async fn an_undeclared_json_flag_is_parsed_and_ignored() {
    // `--json` is reserved framework-wide, so a CLI that dropped its own
    // `--json` flag still accepts the calls agents already have memorized.
    let execution = cli().run_argv(["app", "brain", "--json"]).await;
    let json = parse(&execution);

    assert_eq!(execution.exit_code(), 0);
    assert_eq!(json["ok"], json!(true));
    assert_eq!(json["result"]["json_flag"], json!("true"));
}

#[tokio::test]
async fn an_undeclared_json_flag_does_not_swallow_the_next_positional() {
    // The edge case that would silently break back-compat: `--json` is a
    // reserved *boolean*, so `myrepo` stays a positional instead of becoming
    // the flag's value.
    let execution = cli().run_argv(["app", "brain", "--json", "myrepo"]).await;
    let json = parse(&execution);

    assert_eq!(json["ok"], json!(true));
    assert_eq!(json["result"]["positionals"], json!(["myrepo"]));
}

#[tokio::test]
async fn a_json_flag_before_the_command_name_still_resolves_the_command() {
    let execution = cli().run_argv(["app", "--json", "brain", "myrepo"]).await;
    let json = parse(&execution);

    assert_eq!(json["ok"], json!(true));
    assert_eq!(json["result"]["positionals"], json!(["myrepo"]));
}

#[tokio::test]
async fn the_root_tree_documents_the_json_flag() {
    let execution = cli().run_argv(["app"]).await;
    let json = parse(&execution);
    let flags = json["result"]["agent_flags"].as_array().expect("flags");

    assert!(
        flags.iter().any(|f| f["flag"] == json!("--json")),
        "{flags:?}"
    );
}

#[tokio::test]
async fn a_successful_command_exits_zero_whatever_it_reports() {
    // A degraded backing service is still a successful report. Nothing in the
    // result — not `"healthy": false`, not an error string — can move the exit
    // code; only `CommandOutput::exit_code` can.
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("brain", "Report state")
            .usage("app brain")
            .handler(|_req, _ctx| {
                Box::pin(async move {
                    Ok(CommandOutput::new(json!({
                        "viewer": "unreachable",
                        "error": "connection refused",
                        "healthy": false,
                    })))
                })
            }),
    );
    let execution = cli.run_argv(["app", "brain"]).await;

    assert_eq!(execution.exit_code(), 0);
    assert_eq!(parse(&execution)["ok"], json!(true));
}

#[tokio::test]
async fn a_command_may_report_success_with_a_chosen_exit_code() {
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("check", "Check")
            .usage("app check")
            .handler(|_req, _ctx| {
                Box::pin(async move {
                    Ok(CommandOutput::new(json!({ "stale": true })).exit_code(ExitCode::NOT_FOUND))
                })
            }),
    );
    let execution = cli.run_argv(["app", "check"]).await;

    assert_eq!(execution.exit_code(), 3);
    assert_eq!(parse(&execution)["ok"], json!(true));
}

#[tokio::test]
async fn a_failing_doctor_check_carries_its_own_exit_code() {
    let cli = AgentCli::new("app", "Test CLI").doctor(vec![
        Check::new("auth", || {
            Box::pin(async { CheckResult::fail("no token", "Run `app login`") })
        })
        .exit_code(ExitCode::AUTH),
    ]);
    let execution = cli.run_argv(["app", "doctor"]).await;
    let json = parse(&execution);

    assert_eq!(execution.exit_code(), 4);
    // The report itself succeeded, so the envelope is still ok.
    assert_eq!(json["ok"], json!(true));
    assert_eq!(json["result"]["healthy"], json!(false));
    assert_eq!(json["result"]["checks"][0]["status"], json!("fail"));
    assert_eq!(json["result"]["checks"][0]["fix"], json!("Run `app login`"));
}

#[tokio::test]
async fn a_skipped_doctor_check_is_neither_pass_nor_fail() {
    let cli = AgentCli::new("app", "Test CLI").doctor(vec![
        Check::new("git", || {
            Box::pin(async { CheckResult::pass_with("2.44.0") })
        }),
        Check::new("s3", || {
            Box::pin(async { CheckResult::skip("no bucket configured") })
        })
        .exit_code(ExitCode::API),
    ]);
    let execution = cli.run_argv(["app", "doctor"]).await;
    let json = parse(&execution);
    let checks = &json["result"]["checks"];

    // A skipped check cannot fail the run, and its exit code never applies.
    assert_eq!(execution.exit_code(), 0);
    assert_eq!(json["result"]["healthy"], json!(true));
    assert_eq!(json["result"]["skipped"], json!(1));
    assert_eq!(checks[0]["status"], json!("pass"));
    assert_eq!(checks[1]["status"], json!("skip"));
    assert_eq!(checks[1]["detail"], json!("no bucket configured"));
    // …but it is not reported as a pass either.
    assert_ne!(checks[1]["status"], checks[0]["status"]);
}

#[tokio::test]
async fn quiet_reaches_the_handler_and_still_yields_a_valid_envelope() {
    let execution = cli().run_argv(["app", "brain", "--quiet"]).await;
    let json = parse(&execution);

    assert_eq!(json["result"]["quiet"], json!(true));
    assert_eq!(json["next_actions"], json!([]));
    assert_eq!(execution.exit_code(), 0);
}
