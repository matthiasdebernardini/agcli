//! The usage string is the command's schema: its `<...>` placeholders bound
//! the positional arity and become `next_action` params.

use agcli::{AgentCli, Command, CommandOutput};
use serde_json::{Value, json};

#[tokio::test]
async fn extra_arg_lists_only_the_surplus_arguments() {
    // One required and one optional positional: of three arguments, only the
    // third is surplus, so only it is named.
    let cli = AgentCli::new("myprog", "Arity test CLI")
        .version("0.0.1")
        .clock(|| 0)
        .command(
            Command::new("run", "Run something")
                .usage("<name> [<mode>]")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({"done": true}))) }),
        );

    let exec = cli
        .run(vec![
            "myprog".to_string(),
            "run".to_string(),
            "alpha".to_string(),
            "beta".to_string(),
            "gamma".to_string(),
        ])
        .await;

    let json_str = exec.to_json();
    let envelope: Value = serde_json::from_str(&json_str).expect("output must be valid JSON");

    assert_eq!(envelope["ok"], false, "must fail with extra-args error");

    let message = envelope["error"]["message"]
        .as_str()
        .expect("error.message must be a string");

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

#[tokio::test]
async fn a_bracketed_placeholder_at_usage_index_zero_is_an_optional_param() {
    // The usage starts with `[` at index 0: the root tree's next_action for
    // the command still carries `id`, as an optional param.
    let cli = AgentCli::new("myprog", "Usage index-0 test CLI")
        .version("0.0.1")
        .clock(|| 0)
        .command(
            Command::new("get", "Get something by optional id")
                .usage("[<id>]")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({"ok": true}))) }),
        );

    // The root tree builds a next_action from every command's usage string.
    let exec = cli.run(Vec::<String>::new()).await;

    let json_str = exec.to_json();
    let root: Value = serde_json::from_str(&json_str).expect("root output must be valid JSON");

    assert_eq!(root["ok"], true, "root tree must succeed");

    let next_actions = root["next_actions"]
        .as_array()
        .expect("root envelope must have a next_actions array");

    assert!(
        !next_actions.is_empty(),
        "root next_actions must not be empty"
    );

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

    let id_param = &params["id"];
    let required = id_param
        .get("required")
        .and_then(serde_json::Value::as_bool);
    assert!(
        required == Some(false),
        "'id' param from a bracketed placeholder must be optional (required: false); got: {id_param}"
    );
}
