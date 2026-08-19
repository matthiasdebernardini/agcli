//! `CommandError::data` — the structured half of a failure.
//!
//! `message` and `fix` are prose. Facts an agent must act on — which rows were
//! rejected and why, what the server replied — belong in `data`, not smuggled
//! into the message text for the caller to parse back out. The key is absent
//! from the envelope when the handler never sets it, so an error that has
//! nothing structured to add keeps the shape it always had.

use agcli::{AgentCli, Command, CommandError, Envelope, ExitCode};
use serde_json::{Value, json};

fn cli() -> AgentCli {
    AgentCli::new("app", "Test CLI")
        .command(
            Command::new("post", "Post annotations")
                .usage("app post")
                .handler(|_req, _ctx| {
                    Box::pin(async {
                        Err(CommandError::new(
                            "2 of 3 comments rejected",
                            "PARTIAL_REJECT",
                            "Fix the line numbers and re-post the rejected comments",
                        )
                        .data(json!({
                            "rejected": [
                                { "line": 91, "why": "past end of file" },
                                { "line": 0, "why": "line numbers start at 1" }
                            ]
                        }))
                        .exit_code(ExitCode::USAGE))
                    })
                }),
        )
        .command(
            Command::new("plain", "Fail without a payload")
                .usage("app plain")
                .handler(|_req, _ctx| {
                    Box::pin(async {
                        Err(CommandError::new("nope", "FAILED", "Try again"))
                            as Result<agcli::CommandOutput, CommandError>
                    })
                }),
        )
}

fn json_of(execution: &agcli::Execution) -> Value {
    serde_json::from_str(&execution.to_json()).expect("envelope is valid JSON")
}

#[tokio::test]
async fn the_payload_rides_in_the_error_envelope() {
    let run = cli().run_argv(["app", "post"]).await;
    let Envelope::Error(envelope) = run.envelope() else {
        panic!("expected error envelope");
    };
    assert_eq!(envelope.data.as_ref().unwrap()["rejected"][0]["line"], 91);

    let wire = json_of(&run);
    assert_eq!(wire["ok"], json!(false));
    assert_eq!(wire["error"]["code"], json!("PARTIAL_REJECT"));
    assert_eq!(
        wire["data"]["rejected"][1]["why"],
        json!("line numbers start at 1")
    );
    assert_eq!(run.exit_code(), ExitCode::USAGE);
}

#[tokio::test]
async fn an_error_without_a_payload_has_no_data_key() {
    let run = cli().run_argv(["app", "plain"]).await;
    let wire = json_of(&run);
    assert!(
        wire.get("data").is_none(),
        "unset data must not appear in the envelope: {wire}"
    );
    assert_eq!(run.exit_code(), ExitCode::ERROR);
}

#[tokio::test]
async fn framework_errors_carry_no_payload() {
    // Parse-time failures the framework raises itself (unknown command, bad
    // flag) have no handler payload to carry.
    let run = cli().run_argv(["app", "nope"]).await;
    assert!(json_of(&run).get("data").is_none());
    assert_eq!(run.exit_code(), ExitCode::USAGE);
}
