//! `AgentCli::doctor_with` — a `doctor` command the consumer owns.
//!
//! The framework supplies the check-running handler and the health envelope;
//! the [`Command`] — its name, description, usage string and therefore its
//! declared flags — comes from the caller. These tests pin the contract that
//! made it necessary: a `doctor` that takes `--profile <name>` must accept the
//! flag and hand it to the checks, and the sugar `doctor(checks)` must keep
//! behaving exactly as it did.

use agcli::{AgentCli, Check, CheckResult, Command, Envelope, ExitCode};
use serde_json::json;

/// A `doctor` that declares `--profile` and reports which profile it checked —
/// the nashcode shape: verify the named profile, or the active one.
fn profile_cli() -> AgentCli {
    AgentCli::new("app", "Test CLI").doctor_with(
        Command::new("doctor", "Check the active or named profile")
            .usage("app doctor [--profile=<name>]"),
        vec![
            Check::with_request("profile", |req| {
                let profile = req.flag("profile").unwrap_or("active").to_string();
                Box::pin(async move { CheckResult::pass_with(format!("checked {profile}")) })
            })
            .exit_code(ExitCode::AUTH),
        ],
    )
}

fn success(execution: &agcli::Execution) -> &agcli::SuccessEnvelope {
    match execution.envelope() {
        Envelope::Success(envelope) => envelope,
        Envelope::Error(envelope) => panic!("expected success envelope, got {:?}", envelope.error),
    }
}

fn first_detail(execution: &agcli::Execution) -> String {
    success(execution).result["checks"][0]["detail"]
        .as_str()
        .expect("check entry has a detail")
        .to_string()
}

#[tokio::test]
async fn a_declared_flag_reaches_the_check() {
    let cli = profile_cli();
    // Before the command name — the form that used to exit 2 UNKNOWN_FLAG,
    // because the hardcoded usage string was the flag schema.
    let run = cli.run_argv(["app", "--profile", "ci", "doctor"]).await;
    assert_eq!(run.exit_code(), ExitCode::SUCCESS);
    assert_eq!(success(&run).result["healthy"], json!(true));
    assert_eq!(first_detail(&run), "checked ci");

    // After it, and in `--flag=value` form: same flag, same check.
    let run = cli.run_argv(["app", "doctor", "--profile=ci"]).await;
    assert_eq!(run.exit_code(), ExitCode::SUCCESS);
    assert_eq!(first_detail(&run), "checked ci");
}

#[tokio::test]
async fn an_absent_flag_leaves_the_check_its_default() {
    let run = profile_cli().run_argv(["app", "doctor"]).await;
    assert_eq!(run.exit_code(), ExitCode::SUCCESS);
    assert_eq!(first_detail(&run), "checked active");
}

#[tokio::test]
async fn an_undeclared_flag_is_still_rejected() {
    // The sugar `doctor(checks)` declares no flags, so unknown-flag rejection
    // must still fire: declaring the flag is what makes it legal, not the mere
    // existence of a request-aware check.
    let cli = AgentCli::new("app", "Test CLI").doctor(vec![Check::new("ping", || {
        Box::pin(async { CheckResult::pass() })
    })]);
    let run = cli.run_argv(["app", "--profile", "ci", "doctor"]).await;
    let Envelope::Error(envelope) = run.envelope() else {
        panic!("expected error envelope");
    };
    assert_eq!(envelope.error.code, "UNKNOWN_FLAG");
    assert_eq!(run.exit_code(), ExitCode::USAGE);
}

#[tokio::test]
async fn a_request_check_reads_positionals_too() {
    let cli = AgentCli::new("app", "Test CLI").doctor_with(
        Command::new("doctor", "Check one subsystem").usage("app doctor [<subsystem>]"),
        vec![Check::with_request("subsystem", |req| {
            let subsystem = req.arg(0).unwrap_or("all").to_string();
            Box::pin(async move { CheckResult::pass_with(subsystem) })
        })],
    );
    let run = cli.run_argv(["app", "doctor", "storage"]).await;
    assert_eq!(first_detail(&run), "storage");
}

#[tokio::test]
async fn a_failing_request_check_drives_the_exit_code() {
    let cli = AgentCli::new("app", "Test CLI").doctor_with(
        Command::new("doctor", "Check the active or named profile")
            .usage("app doctor [--profile=<name>]"),
        vec![
            Check::with_request("profile", |req| {
                let profile = req.flag("profile").unwrap_or("active").to_string();
                Box::pin(async move {
                    CheckResult::fail(format!("profile {profile} has no token"), "Run `app login`")
                })
            })
            .exit_code(ExitCode::AUTH),
        ],
    );
    let run = cli.run_argv(["app", "doctor", "--profile=ci"]).await;
    // Still an ok:true envelope — the report ran — carrying the typed code.
    assert_eq!(success(&run).result["healthy"], json!(false));
    assert_eq!(run.exit_code(), ExitCode::AUTH);
    assert!(first_detail(&run).contains("ci"));
}

#[tokio::test]
async fn bare_and_request_checks_run_side_by_side() {
    let cli = AgentCli::new("app", "Test CLI").doctor_with(
        Command::new("doctor", "Health").usage("app doctor [--profile=<name>]"),
        vec![
            Check::new("ping", || {
                Box::pin(async { CheckResult::pass_with("pong") })
            }),
            Check::with_request("profile", |req| {
                let profile = req.flag("profile").unwrap_or("active").to_string();
                Box::pin(async move { CheckResult::skip(format!("{profile} not configured")) })
            }),
        ],
    );
    let run = cli.run_argv(["app", "doctor", "--profile=ci"]).await;
    let result = &success(&run).result;
    assert_eq!(result["healthy"], json!(true));
    assert_eq!(result["skipped"], json!(1));
    assert_eq!(result["checks"][0]["detail"], json!("pong"));
    assert_eq!(result["checks"][1]["detail"], json!("ci not configured"));
    assert_eq!(run.exit_code(), ExitCode::SUCCESS);
}

#[tokio::test]
async fn the_consumers_description_and_usage_reach_the_command_tree() {
    // The prose an agent reads about `doctor` is the consumer's, not a
    // framework default it had to smuggle into a `root_field`.
    let run = profile_cli().run_argv(["app"]).await;
    let commands = success(&run).result["commands"]
        .as_array()
        .expect("root lists commands")
        .clone();
    let doctor = commands
        .iter()
        .find(|c| c["name"] == json!("doctor"))
        .expect("doctor is in the tree");
    assert_eq!(
        doctor["description"],
        json!("Check the active or named profile")
    );
    assert_eq!(doctor["usage"], json!("app doctor [--profile=<name>]"));
}

#[tokio::test]
async fn the_command_may_be_named_anything() {
    let cli = AgentCli::new("app", "Test CLI").doctor_with(
        Command::new("health", "Run environment health checks"),
        vec![Check::new("ping", || {
            Box::pin(async { CheckResult::pass() })
        })],
    );
    let run = cli.run_argv(["app", "health"]).await;
    assert_eq!(success(&run).result["healthy"], json!(true));

    // No usage string given, so the framework fills in `<cli> <name>` — which
    // keeps flag validation on rather than silently disabling it.
    let run = cli.run_argv(["app"]).await;
    let commands = success(&run).result["commands"].clone();
    let health = commands
        .as_array()
        .and_then(|c| c.iter().find(|c| c["name"] == json!("health")))
        .expect("health is in the tree")
        .clone();
    assert_eq!(health["usage"], json!("app health"));
}

#[tokio::test]
async fn the_report_is_still_exempt_from_narrowing() {
    // `doctor_with` keeps the projection opt-out: `--select` must not strip the
    // per-check `fix` out of an unhealthy report.
    let cli = AgentCli::new("app", "Test CLI").doctor_with(
        Command::new("doctor", "Health").usage("app doctor [--profile=<name>]"),
        vec![
            Check::with_request("auth", |_req| {
                Box::pin(async { CheckResult::fail("no token", "Set API_TOKEN") })
            })
            .exit_code(ExitCode::AUTH),
        ],
    );
    let run = cli
        .run_argv(["app", "doctor", "--profile=ci", "--select=healthy"])
        .await;
    let result = &success(&run).result;
    assert_eq!(result["checks"][0]["fix"], json!("Set API_TOKEN"));
    assert_eq!(run.exit_code(), ExitCode::AUTH);
}

#[tokio::test]
async fn a_doctor_command_passes_the_static_audit() {
    let report = profile_cli().audit();
    assert!(report.is_clean(), "{:?}", report.findings);
}

#[tokio::test]
async fn checks_run_in_registration_order() {
    let cli = AgentCli::new("app", "Test CLI").doctor_with(
        Command::new("doctor", "Health").usage("app doctor"),
        vec![
            Check::new("first", || Box::pin(async { CheckResult::pass() })),
            Check::with_request("second", |_req| Box::pin(async { CheckResult::pass() })),
            Check::new("third", || Box::pin(async { CheckResult::pass() })),
        ],
    );
    let run = cli.run_argv(["app", "doctor"]).await;
    let names: Vec<&str> = success(&run).result["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert_eq!(names, ["first", "second", "third"]);
}
