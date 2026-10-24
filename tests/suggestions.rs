//! Did-you-mean suggestions on an unknown command: offered only for a
//! plausible typo (edit distance at most 2, and less than the name's length).
//! The `Valid commands:` list follows either way, so these assert on the
//! conditional `Did you mean` prefix, never on a name the list always holds.

use agcli::{AgentCli, Command, CommandOutput};
use serde_json::{Value, json};

#[tokio::test]
async fn three_deletions_away_is_too_far_to_suggest() {
    // lev("aaabxy", "bxy") = 3: past the gate, so no suggestion.
    let cli = AgentCli::new("prog", "Suggestion test CLI")
        .clock(|| 0)
        .command(
            Command::new("bxy", "A three-letter command")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({"ok": true}))) }),
        );

    let exec = cli.run(["prog", "aaabxy"]).await;
    let raw = exec.to_json();
    let envelope: Value = serde_json::from_str(&raw).expect("envelope must be valid JSON");

    assert_eq!(
        envelope["ok"], false,
        "expected error envelope for unknown command 'aaabxy', got: {envelope}"
    );
    assert_eq!(
        envelope["error"]["code"], "UNKNOWN_COMMAND",
        "expected UNKNOWN_COMMAND code, got: {envelope}"
    );

    let fix = envelope["fix"]
        .as_str()
        .expect("fix field must be present on UNKNOWN_COMMAND error");

    assert!(
        !fix.contains("Did you mean"),
        "expected no did-you-mean suggestion (lev dist 3 > threshold 2), but got: {fix:?}"
    );
}

#[tokio::test]
async fn one_extra_trailing_letter_is_suggested() {
    // lev("statuss", "status") = 1: inside the gate. ("stats" would not tell
    // a correct distance from a subtly wrong one; "statuss" does.)
    let cli = AgentCli::new("prog", "p").clock(|| 0).command(
        Command::new("status", "Show status")
            .handler(|_req| async move { Ok(CommandOutput::new(json!({"ok": true}))) }),
    );

    let exec = cli.run(["prog", "statuss"]).await;
    let raw = exec.to_json();
    let envelope: Value = serde_json::from_str(&raw).expect("envelope must be valid JSON");

    let fix = envelope["fix"]
        .as_str()
        .expect("fix field must be present on UNKNOWN_COMMAND error");

    assert!(
        fix.contains("Did you mean `status`"),
        "expected did-you-mean suggestion for `status` in fix field, got: {fix:?}"
    );
}
