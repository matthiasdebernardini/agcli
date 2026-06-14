// Kills mutant: src/cli.rs:845:44 replace + with - in next_action_from_usage
//
// The `+ -> -` swap makes the slice start at `i-1` instead of `i+1`.
// When the '[' of a bracketed placeholder sits at index 0 (e.g. usage "[<id>]"),
// `i - 1` underflows a usize → panic / slice out-of-bounds during root-tree
// construction. The real code extracts an optional "id" param correctly.
//
// The sibling `+ -> *` mutant (i*1 == i) is an accepted EQUIVALENT mutant
// and is deliberately NOT targeted here.

use agcli::{AgentCli, Command, CommandOutput};
use serde_json::{Value, json};

#[tokio::test]
async fn kill_mutant_src_cli_rs_845() {
    // Pin timestamp for determinism (nextest gives each test its own process).
    // SAFETY: no other thread is reading the environment at this point.
    unsafe { std::env::set_var("SOURCE_DATE_EPOCH", "0") };

    let cli = AgentCli::new("myprog", "Test prog for F2 mutant kill")
        .version("0.0.1")
        .command(
            // Usage starts with '[' at position 0 — this is the key condition.
            // Real code: bracket_content = &usage[0+1 .. 0+close] → "[<id>]" inner → "id"
            // Mutant:    bracket_content = &usage[0-1 .. 0+close] → usize underflow → panic
            Command::new("get", "Get something by optional id")
                .usage("[<id>]")
                .handler(|_req, _ctx| {
                    Box::pin(async move { Ok(CommandOutput::new(json!({"ok": true}))) })
                }),
        );

    // Run the ROOT (no args) — this triggers next_action_from_usage for every
    // registered command's usage string, including our "[<id>]" command.
    // Under the mutant this panics; under real code it completes normally.
    let exec = cli.run_argv(Vec::<String>::new()).await;

    let json_str = exec.to_json();
    let root: Value = serde_json::from_str(&json_str).expect("root output must be valid JSON");

    // Sanity: the root envelope must be ok.
    assert_eq!(root["ok"], true, "root tree must succeed");

    // The root envelope's top-level next_actions list contains one entry per
    // registered command (built from each command's usage string via
    // next_action_from_usage). Find the entry whose command template contains
    // the "id" placeholder extracted from usage "[<id>]".
    let next_actions = root["next_actions"]
        .as_array()
        .expect("root envelope must have a next_actions array");

    assert!(
        !next_actions.is_empty(),
        "root next_actions must not be empty"
    );

    // Find the action for the "get" command — the command field is the usage
    // string "[<id>]" and its params must include "id".
    let get_action = next_actions
        .iter()
        .find(|a| a["params"].get("id").is_some())
        .expect("a next_action with an 'id' param must appear in the root envelope");

    let params = &get_action["params"];
    assert!(
        params.get("id").is_some(),
        "next_action params must include 'id' (extracted from '[<id>]' usage); \
         got action: {get_action}"
    );

    // The "id" param must be optional (required: false), not required.
    // Optional bracket `[<id>]` → required: false
    let id_param = &params["id"];
    let required = id_param
        .get("required")
        .and_then(serde_json::Value::as_bool);
    assert!(
        required == Some(false),
        "'id' param from a bracketed placeholder must be optional (required: false); got: {id_param}"
    );
}
