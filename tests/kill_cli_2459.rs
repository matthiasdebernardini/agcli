// Kills: src/cli.rs:2459:24: replace + with - in extract_bool_flag_names
//
// The mutant changes `i += close_rel + 2` to `i += close_rel - 2`.
// For a short bracket like `[-v]`, close_rel == 2, so the mutant does
// `i += 0` and the while-loop never advances — infinite loop / hang.
// cargo-mutants records a TIMEOUT which counts as "killed".
//
// On real code the loop terminates immediately and the test passes.
// The key exercise: a usage string containing `[-v]` (close_rel == 2).

use agcli::{AgentCli, Command, CommandOutput};
use serde_json::json;

/// Build a CLI whose command usage contains a short bracket bool flag `[-v]`
/// (exactly the shape that makes the mutant hang). Invoke the command with
/// `-v` and assert the flag is correctly recognised as a boolean flag (i.e.
/// no positional is consumed as its value, and the result captures the flag).
#[tokio::test]
async fn kill_mutant_src_cli_rs_2459() {
    let cli = AgentCli::new("app", "test").command(
        Command::new("run", "Run something")
            // `[-v]` → close_rel == 2 (the dangerous case for the mutant)
            .usage("app run [-v] <file>")
            .handler(|req, _ctx| {
                let verbose = req.flag("v").is_some();
                let file = req.arg(0).unwrap_or("").to_string();
                Box::pin(async move {
                    Ok(CommandOutput::new(json!({
                        "verbose": verbose,
                        "file": file,
                    })))
                })
            }),
    );

    // Pass `-v` (short bool flag) followed by a positional.
    // On real code: `[-v]` is parsed as a bool flag, so "data.txt" lands
    // in positional slot 0.
    // On the mutant: extract_bool_flag_names loops forever → timeout.
    let run = cli.run_argv(["app", "run", "-v", "data.txt"]).await;
    let envelope: serde_json::Value =
        serde_json::from_str(&run.to_json()).expect("valid JSON envelope");

    assert_eq!(envelope["ok"], json!(true), "command must succeed");
    assert_eq!(
        envelope["result"]["verbose"],
        json!(true),
        "[-v] must be recognised as a bool flag"
    );
    assert_eq!(
        envelope["result"]["file"],
        json!("data.txt"),
        "positional must not be consumed as flag value"
    );
}
