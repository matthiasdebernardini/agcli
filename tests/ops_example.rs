//! The shipped `ops` example's streaming `deploy` command, end to end.
//!
//! The example is compiled into this test binary (`mod ops` below), and the
//! test re-executes the binary as `<exe> deploy web` with `AGCLI_TEST_CHILD`
//! set. The harness reads `deploy` and `web` as test-name filters, which
//! select only `ops::deploy`; that test then runs the example's own `main`,
//! whose `run_env` reads the same argv: `ops deploy web`. So the events reach
//! the real stdout live, followed by the one terminal line. The child writes
//! [`CHILD_RAN`] to stderr first, and the parent checks for it once and for
//! the harness's `running 1 test` line, so a child the filter never reached,
//! or a filter that reached a second test too, fails loudly.
//!
//! The example's other commands are checked in-process through its `cli()`.

use std::env;
use std::process::Command as Process;

use serde_json::{Value, json};

#[path = "support/literal_actions.rs"]
mod literal_actions;

/// Set in the re-executed child.
const CHILD: &str = "AGCLI_TEST_CHILD";

/// The line the child writes to stderr before it runs the example.
const CHILD_RAN: &str = "AGCLI_TEST_CHILD_RAN ops_example::deploy";

mod ops {
    include!("../examples/ops.rs");

    /// The subprocess entry point: runs the example. A no-op in a normal
    /// test run.
    #[test]
    fn deploy() {
        use std::io::Write;

        if std::env::var_os(super::CHILD).is_some() {
            // Through the handle, not `eprintln!`: the harness captures the
            // macros.
            writeln!(std::io::stderr(), "{}", super::CHILD_RAN).expect("write the sentinel");
            main();
        }
    }

    #[tokio::test]
    async fn literal_actions_run_as_printed() {
        // `ops echo [args...]` is advertised as the literal `ops echo`.
        let cli = cli();
        let ran = super::literal_actions::literal_actions_run_as_printed(
            "ops",
            async |argv: Vec<String>| {
                let run = cli.run(argv).await;
                serde_json::from_str(&run.to_json()).expect("the envelope is JSON")
            },
        )
        .await;
        assert!(ran.contains("ops echo"), "{ran:?}");
    }

    #[tokio::test]
    async fn logs_refuses_a_missing_source_and_a_bad_line_count() {
        // The example teaches typed errors, not quiet defaults: no `<source>` is
        // MISSING_ARG, and a `--lines` that is not a positive count is
        // INVALID_FLAG.
        for (argv, code) in [
            (&["ops", "logs"][..], "MISSING_ARG"),
            (&["ops", "logs", "worker", "--lines=abc"], "INVALID_FLAG"),
            (&["ops", "logs", "worker", "--lines=0"], "INVALID_FLAG"),
        ] {
            let run = cli().run(argv.iter().copied()).await;
            let envelope: serde_json::Value = serde_json::from_str(&run.to_json()).expect("JSON");
            assert_eq!(
                envelope["error"]["code"],
                json!(code),
                "{argv:?}: {envelope}"
            );
            assert_eq!(envelope["exit_code"], json!(2), "{argv:?}: {envelope}");
        }

        let run = cli().run(["ops", "logs", "errors", "--lines=3"]).await;
        let envelope: serde_json::Value = serde_json::from_str(&run.to_json()).expect("JSON");
        assert_eq!(envelope["ok"], json!(true), "{envelope}");
        assert_eq!(envelope["result"]["lines"], json!(3));
        assert_eq!(envelope["result"]["entries"][0], json!("[errors] line-117"));
        let path = envelope["result"]["full_output"]
            .as_str()
            .expect("the full log was written");
        std::fs::remove_file(path).expect("remove the full log");
    }

    #[tokio::test]
    async fn a_value_flag_before_the_command_is_given_no_value() {
        // `logs` is the command, never the value of `--lines`, so `--lines`
        // was given nothing, as it is at the end of the line.
        for argv in [
            ["ops", "--lines", "logs", "worker"],
            ["ops", "logs", "worker", "--lines"],
        ] {
            let run = cli().run(argv).await;
            let envelope: serde_json::Value = serde_json::from_str(&run.to_json()).expect("JSON");
            assert_eq!(
                envelope["error"]["code"],
                json!("INVALID_FLAG"),
                "{argv:?}: {envelope}"
            );
            assert_eq!(
                envelope["error"]["message"],
                json!("flag --lines was given no value"),
                "{argv:?}: {envelope}"
            );
            assert_eq!(envelope["exit_code"], json!(2), "{argv:?}: {envelope}");
        }
    }
}

#[test]
fn streams_steps_and_progress_then_one_terminal_line() {
    let output = Process::new(env::current_exe().expect("current exe"))
        .args(["deploy", "web"])
        .env(CHILD, "1")
        .output()
        .expect("spawn the example");
    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches(CHILD_RAN).count(),
        1,
        "the example did not run exactly once: {stderr}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("running 1 test\n"),
        "the filter selected other tests too: {stdout}"
    );
    let lines: Vec<Value> = stdout
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).expect("each line is JSON"))
        .collect();
    // start, then started/completed/progress for each of the three steps,
    // then the terminal line.
    assert_eq!(lines.len(), 11, "{stdout}");
    assert_eq!(lines[0]["type"], json!("start"));
    assert_eq!(lines[0]["command"], json!("ops deploy web"));
    let (terminal, events) = lines.split_last().expect("lines");
    for event in events {
        assert!(event.get("ok").is_none(), "only the last line is terminal");
        assert!(event["ts"].is_string(), "{event}");
    }
    let progress: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == json!("progress"))
        .map(|event| &event["percent"])
        .collect();
    assert_eq!(progress, [&json!(33), &json!(66), &json!(100)]);

    assert_eq!(terminal["type"], json!("result"));
    assert_eq!(terminal["ok"], json!(true));
    assert_eq!(terminal["result"]["service"], json!("web"));
    assert_eq!(
        terminal["result"]["steps"],
        json!(["build", "upload", "activate"])
    );
}
