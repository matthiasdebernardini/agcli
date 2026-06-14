// Kills mutant: src/cli.rs:1617:55 replace + with - in AgentCli::run_argv_with_context
//
// Line 1617: `let extras = &resolved.remaining[required + optional..];`
// With `+` swapped to `-`, the slice start becomes `required - optional`.
// For required=1, optional=1: real index=2, mutant index=0.
// The "extras" slice then contains ALL positional args (not just the surplus),
// so the error message lists different argument values.

use agcli::{AgentCli, Command, CommandOutput};
use serde_json::{Value, json};

#[tokio::test]
async fn kill_mutant_src_cli_rs_1617() {
    unsafe { std::env::set_var("SOURCE_DATE_EPOCH", "0") };

    let cli = AgentCli::new("myprog", "Test prog for cli.rs:1617 mutant kill")
        .version("0.0.1")
        .command(
            // 1 required + 1 optional positional arg.
            // We invoke with 3 args: "alpha" "beta" "gamma".
            // required=1, optional=1 → required+optional=2 → extras = remaining[2..] = ["gamma"]
            // With mutant (required-optional=0): extras = remaining[0..] = ["alpha","beta","gamma"]
            Command::new("run", "Run something")
                .usage("<name> [<mode>]")
                .handler(|_req, _ctx| {
                    Box::pin(async move { Ok(CommandOutput::new(json!({"done": true}))) })
                }),
        );

    let exec = cli
        .run_argv(vec![
            "myprog".to_string(),
            "run".to_string(),
            "alpha".to_string(),
            "beta".to_string(),
            "gamma".to_string(),
        ])
        .await;

    let json_str = exec.to_json();
    let envelope: Value = serde_json::from_str(&json_str).expect("output must be valid JSON");

    // Must be an error (too many positional args).
    assert_eq!(envelope["ok"], false, "must fail with extra-args error");

    let message = envelope["error"]["message"]
        .as_str()
        .expect("error.message must be a string");

    // Real code: extras = ["gamma"] → message contains "gamma" but NOT "alpha"/"beta"
    // Mutant:    extras = ["alpha","beta","gamma"] → message contains "alpha"
    assert!(
        message.contains("\"gamma\""),
        "error message must list the actual extra arg 'gamma'; got: {message}"
    );
    assert!(
        !message.contains("\"alpha\""),
        "error message must NOT list 'alpha' (it is a required arg, not extra); got: {message}"
    );
    assert!(
        !message.contains("\"beta\""),
        "error message must NOT list 'beta' (it is the optional arg, not extra); got: {message}"
    );
}
