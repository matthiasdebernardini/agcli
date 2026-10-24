//! The shipped `calc` example, compiled into this test binary (`mod calc`
//! below) and checked in-process through its `cli()`.
//!
//! One test also runs it end to end: it re-executes this binary as
//! `<exe> add 3 5` with `AGCLI_TEST_CHILD` set. The harness reads `add`, `3`
//! and `5` as test-name filters, which select only `calc::add`; that test runs
//! the example's own `main`, whose `run_env` reads the same argv.

use std::env;
use std::process::Command as Process;

use serde_json::{Value, json};

#[path = "support/literal_actions.rs"]
mod literal_actions;

/// Set in the re-executed child.
const CHILD: &str = "AGCLI_TEST_CHILD";

mod calc {
    include!("../examples/calc.rs");

    /// The subprocess entry point: runs the example. A no-op in a normal
    /// test run.
    #[test]
    fn add() {
        if std::env::var_os(super::CHILD).is_some() {
            main();
        }
    }

    #[tokio::test]
    async fn literal_actions_run_as_printed() {
        let cli = cli();
        let ran = super::literal_actions::literal_actions_run_as_printed(
            "calc",
            async |argv: Vec<String>| {
                let run = cli.run(argv).await;
                serde_json::from_str(&run.to_json()).expect("the envelope is JSON")
            },
        )
        .await;
        assert!(ran.contains("calc"), "{ran:?}");
    }
}

#[test]
fn the_binary_prints_one_envelope_and_exits_with_its_code() {
    let output = Process::new(env::current_exe().expect("current exe"))
        .args(["add", "3", "5"])
        .env(CHILD, "1")
        .output()
        .expect("spawn the example");
    assert_eq!(output.status.code(), Some(0));

    // The harness prints its own `running 1 test` line first.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<Value> = stdout
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).expect("each line is JSON"))
        .collect();
    assert_eq!(lines.len(), 1, "{stdout}");
    assert_eq!(
        lines[0]["result"],
        json!({ "operation": "add", "result": 8.0 })
    );
    assert_eq!(
        lines[0]["next_actions"][1]["command"],
        json!("calc sub <a> <b>")
    );
}
