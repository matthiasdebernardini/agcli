// Kills both surviving mutants in src/envelope.rs line 61 (Howard Hinnant civil-from-days):
//   1. src/envelope.rs:61:49: replace - with / in rfc3339_utc
//   2. src/envelope.rs:61:34: replace + with - in rfc3339_utc
//
// Epoch rationale: epoch 5184000 = 1970-03-02T00:00:00Z on real code.
// Both mutants corrupt the date-of-era calculation and produce the impossible
// "1970-02-30T00:00:00Z". Common epochs (2020, 2024) do NOT diverge.
// The exact string is pinned so any divergence turns the test red.

use agcli::{AgentCli, Command, CommandOutput};
use serde_json::{Value, json};

#[tokio::test]
async fn kill_mutant_src_envelope_rs_61() {
    // SAFETY: nextest runs each test in its own process; no other thread
    // is reading the environment concurrently at this point.
    unsafe { std::env::set_var("SOURCE_DATE_EPOCH", "5184000") };

    let cli = AgentCli::new("t", "t")
        .command(Command::new("ping", "p").handler(|_r, _c| {
            Box::pin(async move { Ok(CommandOutput::new(json!({"ok": true}))) })
        }));

    let exec = cli.run_argv(["ping"]).await;
    let env: Value = serde_json::from_str(&exec.to_json()).unwrap();

    assert_eq!(
        env["timestamp"], "1970-03-02T00:00:00Z",
        "timestamp must be 1970-03-02; mutants produce impossible 1970-02-30"
    );
}
