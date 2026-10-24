//! The default clock reads `SOURCE_DATE_EPOCH`, and a malformed value fails
//! the run instead of falling back to the wall clock.
//!
//! The variable belongs to the process, and tests never mutate their own
//! environment, so each case runs in a subprocess: the test binary
//! re-executes itself with `child` as its only argument (which makes the
//! harness run just the `child` test below) and the case's
//! `SOURCE_DATE_EPOCH` set on the child alone. `AGCLI_TEST_CHILD` names the
//! case. The child CLI's one command is also named `child`: the harness's
//! filter argument is the CLI's argv too.
//!
//! The harness prints its own `running 1 test` line on stdout first, so the
//! assertions read only the JSON line. The child writes [`CHILD_RAN`] to
//! stderr as it starts, and the parent checks for it once and for the
//! `running 1 test` line, so a child the filter never reached, or a filter
//! that reached a second test too, fails instead of passing by accident.

use std::env;
use std::io::{self, Write};
use std::process::{Command as Process, Output};

use agcli::{AgentCli, Command, CommandOutput, Timestamp};
use serde_json::{Value, json};
use tokio::runtime::Runtime;

/// Names the case the `child` test runs.
const CHILD_CASE: &str = "AGCLI_TEST_CHILD";

/// The line the child writes to stderr as it starts its case.
const CHILD_RAN: &str = "AGCLI_TEST_CHILD_RAN clock::child";

/// The subprocess entry point. A no-op in a normal test run.
#[test]
fn child() {
    let Ok(case) = env::var(CHILD_CASE) else {
        return;
    };
    // Through the handle, not `eprintln!`: the harness captures the macros.
    writeln!(io::stderr(), "{CHILD_RAN}").expect("write the sentinel");
    let cli = AgentCli::new("app", "Clock test CLI").command(
        Command::new("child", "The command under test")
            .usage("app child")
            .handler(|_req| async move { Ok(CommandOutput::new(json!({ "ran": true }))) }),
    );
    let cli = match case.as_str() {
        "default" => cli,
        "custom" => cli.clock(|| 0),
        other => panic!("unknown child case {other:?}"),
    };
    let runtime = Runtime::new().expect("runtime");
    runtime.block_on(cli.run_env()).finish()
}

/// Re-run this test binary as the `case` child with `SOURCE_DATE_EPOCH`
/// set to `source_date_epoch`, and check that the `child` test alone ran
/// and reached its case.
fn run_case(case: &str, source_date_epoch: &str) -> Output {
    let output = Process::new(env::current_exe().expect("current exe"))
        .arg("child")
        .env(CHILD_CASE, case)
        .env("SOURCE_DATE_EPOCH", source_date_epoch)
        .output()
        .expect("spawn the child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("running 1 test\n"),
        "the `child` filter selected other tests too: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches(CHILD_RAN).count(),
        1,
        "the child case did not run exactly once: {stderr}"
    );
    output
}

/// The one JSON envelope the child printed.
fn envelope(output: &Output) -> Value {
    let lines: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect();
    assert_eq!(lines.len(), 1, "exactly one envelope: {lines:?}");
    lines.into_iter().next().expect("one envelope")
}

#[test]
fn source_date_epoch_pins_the_timestamp() {
    let output = run_case("default", "1740000000");
    let envelope = envelope(&output);

    assert_eq!(output.status.code(), Some(0), "{envelope}");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(envelope["timestamp"], json!("2025-02-19T21:20:00Z"));
}

#[test]
fn a_malformed_source_date_epoch_is_an_invalid_env_error() {
    for bad in ["-5", "yesterday", "", "1.5"] {
        let output = run_case("default", bad);
        let envelope = envelope(&output);

        assert_eq!(output.status.code(), Some(2), "{bad:?}: {envelope}");
        assert_eq!(envelope["ok"], json!(false), "{bad:?}: {envelope}");
        assert_eq!(
            envelope["error"]["code"],
            json!("INVALID_ENV"),
            "{envelope}"
        );
        assert_eq!(envelope["exit_code"], json!(2), "{envelope}");
        assert_eq!(
            envelope["fix"],
            json!("Unset SOURCE_DATE_EPOCH or set it to a Unix timestamp in seconds"),
            "{envelope}"
        );
        assert!(
            envelope["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(&format!("{bad:?}"))),
            "the message names the value: {envelope}"
        );
        // The command never ran: the pinned clock is read before anything.
        assert!(envelope.get("result").is_none(), "{envelope}");
        // Named like any other envelope: the program's file name, not the
        // path argv[0] carried.
        assert_eq!(
            envelope["command"],
            json!(format!("{} child", exe_name())),
            "{envelope}"
        );
    }
}

/// The file name of this test binary, as an envelope's `command` names it.
fn exe_name() -> String {
    env::current_exe()
        .expect("current exe")
        .file_name()
        .and_then(|name| name.to_str())
        .expect("a UTF-8 file name")
        .to_string()
}

#[test]
fn a_custom_clock_replaces_source_date_epoch() {
    let output = run_case("custom", "-5");
    let envelope = envelope(&output);

    assert_eq!(output.status.code(), Some(0), "{envelope}");
    assert_eq!(envelope["timestamp"], json!("1970-01-01T00:00:00Z"));
}

/// A clock reading past `Timestamp::MAX` has no RFC 3339 spelling, so the run
/// panics naming the bound instead of writing a timestamp no reader parses.
#[tokio::test]
#[should_panic(expected = "the clock read 253402300800 seconds, past Timestamp::MAX \
                           (253402300799 = 9999-12-31T23:59:59Z)")]
async fn a_clock_reading_past_the_last_timestamp_panics() {
    let cli = AgentCli::new("app", "x").clock(|| Timestamp::MAX.epoch_secs() + 1);
    let _ = cli.run(["app"]).await;
}
