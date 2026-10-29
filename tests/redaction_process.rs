//! A secret given as an argument stays out of the envelopes the panic hook
//! writes on stdout and stderr.
//!
//! Like `tests/panic_hook.rs`, each case re-executes the test binary with
//! `child` as the test filter. A second filter, `password=hunter2`, rides in
//! the child's real argv (the harness takes it as another name to match, which
//! selects nothing more), so the child CLI's `redact_arg` hook has a secret to
//! mask. `AGCLI_TEST_CHILD` names the case.

use std::env;
use std::io::{self, Write};
use std::process::{Command as Process, Output};

use agcli::{AgentCli, Command, StreamEvent};
use tokio::runtime;

const CHILD_CASE: &str = "AGCLI_TEST_CHILD";
const CHILD_RAN: &str = "AGCLI_TEST_CHILD_RAN redaction_process::child";
const SECRET: &str = "hunter2";

/// The subprocess entry point. A no-op in a normal test run.
#[test]
fn child() {
    let Ok(case) = env::var(CHILD_CASE) else {
        return;
    };
    writeln!(io::stderr(), "{CHILD_RAN}").expect("write the sentinel");
    let command = Command::new("child", "The command under test");
    let command = match case.as_str() {
        "body" => command.handler(|_req| async move { panic!("panic in the body") }),
        "raw" => command.raw_handler(|_args| async move { panic!("panic in the raw handler") }),
        "stream" => command.stream_handler(|_req, out| async move {
            out.emit(&StreamEvent::Start {
                command: "child".to_string(),
            })?;
            panic!("panic in the stream handler");
        }),
        other => panic!("unknown child case {other:?}"),
    };
    let cli = AgentCli::new("app", "Redaction test CLI")
        .clock(|| 0)
        .redact_arg(|arg| {
            let (field, _) = arg.split_once('=')?;
            (field == "password").then(|| "password=<redacted>".to_string())
        })
        .command(command);
    let runtime = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .expect("runtime");
    let execution = runtime
        .block_on(async move { tokio::spawn(async move { cli.run_env().await }).await })
        .expect("the run task completes");
    execution.finish()
}

fn run_case(case: &str) -> Output {
    let output = Process::new(env::current_exe().expect("current exe"))
        .arg("child")
        .arg(format!("password={SECRET}"))
        .env(CHILD_CASE, case)
        .env("RUST_TEST_NOCAPTURE", "1")
        .output()
        .expect("spawn the child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("running 1 test\n"),
        "the filters selected other tests too: {stdout}"
    );
    assert_eq!(stderr.matches(CHILD_RAN).count(), 1, "{stderr}");
    output
}

/// Run `case` and assert the secret is on neither stream and the masked
/// command line is in the envelope, on stderr when `on_stderr`, else stdout.
fn assert_masked(case: &str, on_stderr: bool) {
    let output = run_case(case);
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains(SECRET), "stdout leaked: {stdout}");
    assert!(!stderr.contains(SECRET), "stderr leaked: {stderr}");
    let envelope = if on_stderr { &stderr } else { &stdout };
    assert!(
        envelope.contains(r#"child 'password=<redacted>'","timestamp""#),
        "the envelope names the masked line: {envelope}"
    );
    assert!(envelope.contains("HANDLER_PANIC"), "{envelope}");
}

#[test]
fn a_panic_envelope_on_stdout_is_masked() {
    assert_masked("body", false);
}

#[test]
fn a_panic_envelope_on_stderr_for_a_raw_command_is_masked() {
    assert_masked("raw", true);
}

#[test]
fn a_panic_terminal_line_of_a_stream_is_masked() {
    assert_masked("stream", false);
}
