//! Contract guarantees a CLI built on agcli leans on.
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
            .handler(|req| {
                let positionals = req.positionals().to_vec();
                let json_flag = req.flag("json").map(str::to_string);
                let quiet = req.quiet();
                async move {
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
                }
            }),
    )
}

#[tokio::test]
async fn an_undeclared_json_flag_is_parsed_and_ignored() {
    // `--json` is reserved framework-wide: it is the flag an agent most often
    // guesses on an unfamiliar tool, and output is always JSON anyway.
    let execution = cli().run(["app", "brain", "--json"]).await;
    let json = parse(&execution);

    assert_eq!(execution.exit_code(), ExitCode::SUCCESS);
    assert_eq!(json["ok"], json!(true));
    assert_eq!(json["result"]["json_flag"], json!("true"));
}

#[tokio::test]
async fn an_undeclared_json_flag_does_not_swallow_the_next_positional() {
    // `--json` is a reserved *boolean*, so `myrepo` stays a positional
    // instead of becoming the flag's value.
    let execution = cli().run(["app", "brain", "--json", "myrepo"]).await;
    let json = parse(&execution);

    assert_eq!(json["ok"], json!(true));
    assert_eq!(json["result"]["positionals"], json!(["myrepo"]));
}

#[tokio::test]
async fn a_json_flag_before_the_command_name_still_resolves_the_command() {
    let execution = cli().run(["app", "--json", "brain", "myrepo"]).await;
    let json = parse(&execution);

    assert_eq!(json["ok"], json!(true));
    assert_eq!(json["result"]["positionals"], json!(["myrepo"]));
}

#[tokio::test]
async fn the_root_tree_documents_the_json_flag() {
    let execution = cli().run(["app"]).await;
    let json = parse(&execution);
    let flags = json["result"]["agent_flags"].as_array().expect("flags");

    assert!(
        flags.iter().any(|f| f["flag"] == json!("--json")),
        "{flags:?}"
    );
}

#[tokio::test]
async fn without_reserved_flags_an_undeclared_json_swallows_the_positional() {
    // The other half of the guarantee, pinned so it cannot drift: `--json` is
    // safe *because* it is reserved. Turn the reserved vocabulary off and it
    // becomes an ordinary undeclared flag, which consumes the next token.
    let cli = AgentCli::new("app", "Test CLI")
        .without_reserved_flags()
        .command(
            Command::new("brain", "Report state")
                .usage("app brain [<repo>]")
                .handler(|req| {
                    let positionals = req.positionals().to_vec();
                    let json_flag = req.flag("json").map(str::to_string);
                    async move {
                        Ok(CommandOutput::new(
                            json!({ "positionals": positionals, "json_flag": json_flag }),
                        ))
                    }
                }),
        );
    let execution = cli.run(["app", "brain", "--json", "myrepo"]).await;
    let json = parse(&execution);

    assert_eq!(json["result"]["json_flag"], json!("myrepo"));
    assert_eq!(json["result"]["positionals"], json!([]));
}

#[tokio::test]
async fn a_successful_command_exits_zero_whatever_it_reports() {
    // A degraded backing service is still a successful report. Nothing in the
    // result (not `"healthy": false`, not an error string) can move the exit
    // code; only `CommandOutput::exit_code` can.
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("brain", "Report state")
            .usage("app brain")
            .handler(|_req| async move {
                Ok(CommandOutput::new(json!({
                    "viewer": "unreachable",
                    "error": "connection refused",
                    "healthy": false,
                })))
            }),
    );
    let execution = cli.run(["app", "brain"]).await;

    assert_eq!(execution.exit_code(), ExitCode::SUCCESS);
    assert_eq!(parse(&execution)["ok"], json!(true));
}

#[tokio::test]
async fn a_command_may_report_success_with_a_chosen_exit_code() {
    let cli = AgentCli::new("app", "Test CLI").command(
        Command::new("check", "Check")
            .usage("app check")
            .handler(|_req| async move {
                Ok(CommandOutput::new(json!({ "stale": true })).exit_code(ExitCode::NOT_FOUND))
            }),
    );
    let execution = cli.run(["app", "check"]).await;

    assert_eq!(execution.exit_code(), ExitCode::NOT_FOUND);
    assert_eq!(parse(&execution)["ok"], json!(true));
}

#[tokio::test]
async fn a_failing_doctor_check_carries_its_own_exit_code() {
    let cli = AgentCli::new("app", "Test CLI").doctor(vec![
        Check::new("auth", |_req| async {
            CheckResult::fail("no token", "Run `app login`")
        })
        .exit_code(ExitCode::AUTH),
    ]);
    let execution = cli.run(["app", "doctor"]).await;
    let json = parse(&execution);

    assert_eq!(execution.exit_code(), ExitCode::AUTH);
    // The report itself succeeded, so the envelope is still ok.
    assert_eq!(json["ok"], json!(true));
    assert_eq!(json["result"]["healthy"], json!(false));
    assert_eq!(json["result"]["checks"][0]["status"], json!("fail"));
    assert_eq!(json["result"]["checks"][0]["fix"], json!("Run `app login`"));
}

#[tokio::test]
async fn a_skipped_doctor_check_is_neither_pass_nor_fail() {
    let cli = AgentCli::new("app", "Test CLI").doctor(vec![
        Check::new("git", |_req| async { CheckResult::pass_with("2.44.0") }),
        Check::new("s3", |_req| async {
            CheckResult::skip("no bucket configured")
        })
        .exit_code(ExitCode::API),
    ]);
    let execution = cli.run(["app", "doctor"]).await;
    let json = parse(&execution);
    let checks = &json["result"]["checks"];

    // A skipped check cannot fail the run, and its exit code never applies.
    assert_eq!(execution.exit_code(), ExitCode::SUCCESS);
    assert_eq!(json["result"]["healthy"], json!(true));
    assert_eq!(json["result"]["skipped"], json!(1));
    assert_eq!(checks[0]["status"], json!("pass"));
    assert_eq!(checks[1]["status"], json!("skip"));
    assert_eq!(checks[1]["detail"], json!("no bucket configured"));
    // …but it is not reported as a pass either.
    assert_ne!(checks[1]["status"], checks[0]["status"]);
    // `status` is the one outcome field; there is no coarse `ok` beside it.
    assert!(checks[0].get("ok").is_none(), "{checks:?}");
    assert!(checks[1].get("ok").is_none(), "{checks:?}");
    // A skip carries no `fix`: there is nothing to repair.
    assert!(checks[1].get("fix").is_none(), "{checks:?}");
}

#[test]
fn every_public_type_is_send_and_sync() {
    // A CLI is built once and driven from any thread or executor, and a
    // request, a writer and an execution cross worker threads, so every
    // public type is `Send + Sync`. Pinned here so a refactor cannot regress
    // it in silence.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<agcli::AgentCli>();
    assert_send_sync::<agcli::Command>();
    assert_send_sync::<agcli::CommandRequest>();
    assert_send_sync::<agcli::CommandOutput>();
    assert_send_sync::<agcli::CommandError>();
    assert_send_sync::<agcli::Execution>();
    assert_send_sync::<agcli::Envelope>();
    assert_send_sync::<agcli::SuccessEnvelope>();
    assert_send_sync::<agcli::ErrorEnvelope>();
    assert_send_sync::<agcli::ErrorBody>();
    assert_send_sync::<agcli::NextAction>();
    assert_send_sync::<agcli::ActionParam>();
    assert_send_sync::<agcli::Invocation>();
    assert_send_sync::<agcli::ParseInvocationError>();
    assert_send_sync::<agcli::StreamWriter>();
    assert_send_sync::<agcli::StreamEvent>();
    assert_send_sync::<agcli::StreamWriteError>();
    assert_send_sync::<agcli::StepStatus>();
    assert_send_sync::<agcli::LogLevel>();
    assert_send_sync::<agcli::Timestamp>();
    assert_send_sync::<agcli::ParseTimestampError>();
    assert_send_sync::<agcli::ExitCode>();
    assert_send_sync::<agcli::ErrorCode>();
    assert_send_sync::<agcli::TruncatedEntries>();
    assert_send_sync::<agcli::Check>();
    assert_send_sync::<agcli::CheckResult>();
    assert_send_sync::<agcli::CheckStatus>();
    assert_send_sync::<agcli::AuditReport>();
    assert_send_sync::<agcli::AuditFinding>();
    assert_send_sync::<agcli::AuditSeverity>();
}

#[tokio::test]
async fn quiet_reaches_the_handler_and_still_yields_a_valid_envelope() {
    let execution = cli().run(["app", "brain", "--quiet"]).await;
    let json = parse(&execution);

    assert_eq!(json["result"]["quiet"], json!(true));
    assert_eq!(json["next_actions"], json!([]));
    assert_eq!(execution.exit_code(), ExitCode::SUCCESS);
}
