//! The panic hook `AgentCli::run_env` installs, and printing to a closed
//! stdout.
//!
//! A panic hook ends the process, so each case runs in a subprocess: the test
//! binary re-executes itself with `child` as its only argument, which makes
//! the harness run just the `child` test below, and `AGCLI_TEST_CHILD` names
//! the case. That test builds a tiny CLI whose one command is also named
//! `child` — the harness's filter argument is the CLI's argv too — and runs it
//! with `run_env` on a multi-thread runtime.
//!
//! The harness prints its own `running 1 test` line on stdout first, so the
//! assertions read only the JSON lines.

use std::env;
use std::io::{self, BufRead, BufReader, Read};
use std::process::{self, Command as Process, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use agcli::{AgentCli, Command, CommandOutput, StreamEvent};
use serde_json::{Value, json};
use tokio::runtime::{self, Runtime};

/// Names the case the `child` test runs.
const CHILD_CASE: &str = "AGCLI_TEST_CHILD";

/// The child CLI's clock, in Unix seconds: `0` unless a case moves it.
static CLOCK: AtomicU64 = AtomicU64::new(0);

/// The subprocess entry point. A no-op in a normal test run.
#[test]
fn child() {
    let Ok(case) = env::var(CHILD_CASE) else {
        return;
    };
    let command = Command::new("child", "The command under test");
    let command = match case.as_str() {
        "body" => command.handler(|_req| async move {
            // Give the multi-thread runtime a chance to move the future to
            // another worker before it panics.
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            panic!("panic in the async body")
        }),
        "prologue" => command.handler(|req| {
            assert!(
                req.positionals().len() > 99,
                "panic in the synchronous prologue"
            );
            async move { Ok(CommandOutput::new(json!({}))) }
        }),
        "raw" => command.raw_handler(|_args| async move {
            panic!("panic in the raw handler");
        }),
        "stream" => command.stream_handler(|_req, mut out| async move {
            out.emit(&StreamEvent::Start {
                command: "child".to_string(),
            })?;
            panic!("panic in the stream handler");
        }),
        "late" => command.handler(|_req| async move {
            // The clock moves on while the handler runs; the envelope must
            // carry the reading at the panic, not one from the run's start.
            CLOCK.store(86_400, Ordering::SeqCst);
            panic!("panic after the clock moved")
        }),
        "spawned" => command.handler(|_req| async move {
            let joined = tokio::spawn(async { panic!("panic in a spawned task") }).await;
            Ok(CommandOutput::new(
                json!({ "task_panicked": joined.is_err_and(|e| e.is_panic()) }),
            ))
        }),
        "print" => {
            // Wait until the parent has closed our stdout, then print into it.
            let mut input = String::new();
            io::stdin().read_to_string(&mut input).expect("read stdin");
            let cli = AgentCli::new("app", "x").command(
                Command::new("status", "Status")
                    .handler(|_req| async move { Ok(CommandOutput::new(json!({ "ok": 1 }))) }),
            );
            let runtime = Runtime::new().expect("runtime");
            let code = runtime.block_on(async { cli.run(["app", "status"]).await.print() });
            process::exit(i32::from(code.get()));
        }
        other => panic!("unknown child case {other:?}"),
    };
    let cli = AgentCli::new("app", "Panic hook test CLI")
        .clock(|| CLOCK.load(Ordering::SeqCst))
        .command(command);
    let runtime = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .expect("runtime");
    // Spawned, so a worker thread (not this one) polls the run, and the
    // handler can hop between workers.
    let execution = runtime
        .block_on(async move { tokio::spawn(async move { cli.run_env().await }).await })
        .expect("the run task completes");
    execution.finish()
}

/// Re-run this test binary as the `case` child.
fn run_case(case: &str) -> Output {
    Process::new(env::current_exe().expect("current exe"))
        .arg("child")
        .env(CHILD_CASE, case)
        // Uncaptured, so the previous hook's panic message reaches stderr.
        .env("RUST_TEST_NOCAPTURE", "1")
        .output()
        .expect("spawn the child")
}

/// Every line of `bytes` that is a JSON object.
fn json_lines(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

/// Assert `envelope` is the `HANDLER_PANIC` envelope for the `child` command.
fn assert_handler_panic(envelope: &Value, message: &str) {
    assert_eq!(envelope["ok"], json!(false), "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        json!("HANDLER_PANIC"),
        "{envelope}"
    );
    assert_eq!(
        envelope["error"]["message"],
        json!(format!("handler panicked: {message}")),
        "{envelope}"
    );
    assert_eq!(envelope["exit_code"], json!(1), "{envelope}");
    assert!(
        envelope["command"]
            .as_str()
            .is_some_and(|line| line.ends_with(" child")),
        "the envelope names the run's command line: {envelope}"
    );
    assert!(
        envelope["timestamp"]
            .as_str()
            .is_some_and(|ts| ts.ends_with('Z')),
        "{envelope}"
    );
}

#[test]
fn a_panic_in_the_async_body_writes_the_envelope_on_stdout() {
    let output = run_case("body");
    assert_eq!(output.status.code(), Some(1));
    let lines = json_lines(&output.stdout);
    assert_eq!(lines.len(), 1, "exactly one envelope: {lines:?}");
    assert_handler_panic(&lines[0], "panic in the async body");
    assert!(
        !lines[0]["next_actions"]
            .as_array()
            .expect("array")
            .is_empty(),
        "{}",
        lines[0]
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("panic in the async body"),
        "the previous hook still reports the panic on stderr"
    );
}

#[test]
fn a_panic_in_the_synchronous_prologue_writes_the_envelope() {
    let output = run_case("prologue");
    assert_eq!(output.status.code(), Some(1));
    let lines = json_lines(&output.stdout);
    assert_eq!(lines.len(), 1, "exactly one envelope: {lines:?}");
    assert_handler_panic(&lines[0], "panic in the synchronous prologue");
}

#[test]
fn a_panicking_raw_handler_never_lands_on_stdout() {
    let output = run_case("raw");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        json_lines(&output.stdout).is_empty(),
        "stdout belongs to the raw command: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let lines = json_lines(&output.stderr);
    let envelope = lines.last().expect("the envelope on stderr");
    assert_handler_panic(envelope, "panic in the raw handler");
}

#[test]
fn a_panicking_stream_handler_ends_with_a_terminal_error_line() {
    let output = run_case("stream");
    assert_eq!(output.status.code(), Some(1));
    let lines = json_lines(&output.stdout);
    assert_eq!(lines.len(), 2, "one event, then the terminal: {lines:?}");
    assert_eq!(lines[0]["type"], json!("start"));
    assert_eq!(lines[1]["type"], json!("error"));
    assert_handler_panic(&lines[1], "panic in the stream handler");
}

#[test]
fn the_panic_envelope_reads_the_clock_when_the_panic_happens() {
    let output = run_case("late");
    assert_eq!(output.status.code(), Some(1));
    let lines = json_lines(&output.stdout);
    assert_eq!(lines.len(), 1, "exactly one envelope: {lines:?}");
    assert_handler_panic(&lines[0], "panic after the clock moved");
    assert_eq!(lines[0]["timestamp"], json!("1970-01-02T00:00:00Z"));
}

#[test]
fn a_panic_the_runtime_catches_in_a_spawned_task_is_left_alone() {
    let output = run_case("spawned");
    assert_eq!(output.status.code(), Some(0));
    let lines = json_lines(&output.stdout);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["ok"], json!(true), "{}", lines[0]);
    assert_eq!(lines[0]["result"]["task_panicked"], json!(true));
}

#[test]
fn printing_to_a_closed_stdout_does_not_panic() {
    let mut process = Process::new(env::current_exe().expect("current exe"))
        .arg("child")
        .env(CHILD_CASE, "print")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the child");
    // Let the harness print its `running 1 test` line, then close the pipe
    // while the child waits on stdin, then let it print.
    let mut stdout = BufReader::new(process.stdout.take().expect("stdout"));
    let mut line = String::new();
    while !line.contains("running") {
        line.clear();
        assert_ne!(
            stdout.read_line(&mut line).expect("read"),
            0,
            "child stdout ended early"
        );
    }
    drop(stdout);
    drop(process.stdin.take());

    let output = process.wait_with_output().expect("wait for the child");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}
