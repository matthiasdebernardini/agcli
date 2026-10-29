//! Secrets given on the command line stay out of every envelope: the value of
//! a flag the usage does not declare, a `Command::secret` flag or positional,
//! and whatever the `AgentCli::redact_arg` hook rewrites. The handler still
//! reads the real values.

use agcli::{AgentCli, Command, CommandError, CommandOutput, ErrorCode, Execution, ExitCode};
use serde_json::{Value, json};

const SECRET: &str = "hunter2";

/// A password manager: `store unlock` declares no flags, `login` declares a
/// secret `--token`, `item set` takes a secret `<value>`, `item edit` takes a
/// `field=value` assignment the hook knows about, and `stream` streams.
fn cli() -> AgentCli {
    AgentCli::new("passwarden", "Password store")
        .clock(|| 0)
        .redact_arg(|arg| {
            let (field, _) = arg.split_once('=')?;
            (field == "password").then(|| "password=<redacted>".to_string())
        })
        .command(
            Command::new("store", "Store operations").subcommand(
                Command::new("unlock", "Unlock the store")
                    .usage("passwarden store unlock")
                    .handler(|req| async move {
                        // The handler gets the real value of an undeclared flag.
                        Ok(CommandOutput::new(json!({
                            "seen": req.flag("passphrase").unwrap_or("none"),
                        })))
                    }),
            ),
        )
        .command(
            Command::new("login", "Log in")
                .usage("passwarden login <user> [--token=<value>] [-t <value>] [--format=<f>]")
                .secret("token")
                .secret("t")
                .handler(|req| async move {
                    let token: u32 = req.flag_parse("token")?.unwrap_or(0);
                    let short: u32 = req.flag_parse("t")?.unwrap_or(0);
                    Ok(CommandOutput::new(json!({ "token": token + short })))
                }),
        )
        .command(
            Command::new("item", "Items")
                .subcommand(
                    Command::new("set", "Set an item's value")
                        .usage("passwarden item set <name> <value>")
                        .secret("value")
                        .handler(|req| async move {
                            let value: u32 = req.arg_parse(1, "value")?;
                            Ok(CommandOutput::new(json!({ "value": value })))
                        }),
                )
                .subcommand(
                    Command::new("edit", "Edit an item")
                        .usage("passwarden item edit <name> <assignment>")
                        .handler(|req| async move {
                            let assignment = req.require_arg(1, "assignment")?.to_string();
                            Ok(CommandOutput::new(
                                json!({ "assignment_len": assignment.len() }),
                            ))
                        }),
                )
                .subcommand(
                    Command::new("fail", "Always fails")
                        .usage("passwarden item fail <name>")
                        .handler(|_req| async move {
                            Err(CommandError::new(
                                ErrorCode::new("ALWAYS"),
                                "it failed",
                                "Do not retry.",
                            )
                            .exit_code(ExitCode::new(3)))
                        }),
                ),
        )
        .command(
            Command::new("items", "List items")
                .usage("passwarden items [--limit=<n>]")
                .handler(|_req| async move {
                    Ok(CommandOutput::list(vec![json!({ "id": 1, "name": "a" })]))
                }),
        )
        .command(
            Command::new("watch", "Stream events")
                .usage("passwarden watch <name>")
                .secret("name")
                .stream_handler(|_req, _out| async move { Ok(CommandOutput::new(json!({}))) }),
        )
        .command(
            Command::new("grep", "Search")
                .usage("passwarden grep <pattern>")
                .raw_handler(|_args| async move { ExitCode::SUCCESS }),
        )
}

fn parse(execution: &Execution) -> Value {
    serde_json::from_str(&execution.to_json()).expect("envelope is JSON")
}

/// Everything `execution` would print on stdout.
fn printed(execution: &Execution) -> String {
    let mut text = execution.stream_events().join("\n");
    text.push_str(&execution.to_json());
    text.push_str(&execution.to_json_pretty());
    text
}

/// Run `argv`, assert the secret appears nowhere in what is printed, and
/// return the envelope.
async fn run(argv: &[&str]) -> (Execution, Value) {
    let execution = cli().run(argv.iter().copied()).await;
    assert!(
        !printed(&execution).contains(SECRET),
        "{argv:?} leaked: {}",
        printed(&execution)
    );
    let envelope = parse(&execution);
    (execution, envelope)
}

#[tokio::test]
async fn an_unknown_flag_value_is_never_echoed_in_either_form() {
    for (argv, command) in [
        (
            &["passwarden", "store", "unlock", "--passphrase=hunter2"][..],
            "passwarden store unlock --passphrase=<redacted>",
        ),
        (
            &["passwarden", "store", "unlock", "--passphrase", "hunter2"][..],
            "passwarden store unlock --passphrase <redacted>",
        ),
        (
            &["passwarden", "store", "unlock", "-p=hunter2"][..],
            "passwarden store unlock -p=<redacted>",
        ),
        (
            &["passwarden", "store", "unlock", "-p", "hunter2"][..],
            "passwarden store unlock -p <redacted>",
        ),
    ] {
        let (_, envelope) = run(argv).await;
        assert_eq!(envelope["error"]["code"], json!("UNKNOWN_FLAG"), "{argv:?}");
        assert_eq!(envelope["command"], json!(command));
    }
}

#[tokio::test]
async fn an_unknown_flag_on_a_command_that_runs_is_masked_and_still_reaches_the_handler() {
    // With the flag's check off, the command runs: the handler sees the real
    // value, the envelope does not.
    let cli = AgentCli::new("passwarden", "Password store")
        .clock(|| 0)
        .command(
            Command::new("unlock", "Unlock")
                .usage("passwarden unlock")
                .allow_unknown_flags()
                .handler(|req| async move {
                    Ok(CommandOutput::new(json!({
                        "seen": req.flag("passphrase").unwrap_or("none"),
                    })))
                }),
        );
    let run = cli
        .run(["passwarden", "unlock", "--passphrase", "hunter2"])
        .await;
    let envelope = parse(&run);
    assert_eq!(envelope["result"]["seen"], json!("hunter2"));
    assert_eq!(
        envelope["command"],
        json!("passwarden unlock --passphrase <redacted>")
    );
    assert!(!envelope["command"].as_str().unwrap().contains(SECRET));
}

#[tokio::test]
async fn unknown_flags_are_masked_before_any_command_is_known() {
    for argv in [
        vec!["passwarden", "--passphrase=hunter2"],
        vec!["passwarden", "--passphrase", "hunter2"],
        vec!["passwarden", "bogus", "--passphrase=hunter2"],
        vec!["passwarden", "store", "bogus", "--passphrase=hunter2"],
        vec!["passwarden", "help", "--passphrase=hunter2"],
        vec!["passwarden", "--passphrase=hunter2", "--help"],
        vec!["passwarden", "store", "--passphrase=hunter2"],
        vec!["passwarden", "version", "--passphrase=hunter2"],
    ] {
        let (_, envelope) = run(&argv).await;
        let command = envelope["command"].as_str().expect("command");
        assert!(command.contains("--passphrase"), "{argv:?}: {command}");
        assert!(command.contains("<redacted>"), "{argv:?}: {command}");
    }
}

#[tokio::test]
async fn a_declared_flag_keeps_its_value_and_a_secret_one_loses_it() {
    let (_, envelope) = run(&["passwarden", "login", "ann", "--format=json", "--token=42"]).await;
    assert_eq!(envelope["ok"], json!(true));
    assert_eq!(envelope["result"]["token"], json!(42));
    assert_eq!(
        envelope["command"],
        json!("passwarden login ann --format=json --token=<redacted>")
    );
    for (argv, command) in [
        (
            &["passwarden", "login", "ann", "--token", "7"][..],
            "passwarden login ann --token <redacted>",
        ),
        (
            &["passwarden", "login", "ann", "-t", "7"][..],
            "passwarden login ann -t <redacted>",
        ),
        (
            &["passwarden", "login", "ann", "-t=7"][..],
            "passwarden login ann -t=<redacted>",
        ),
    ] {
        let (_, envelope) = run(argv).await;
        assert_eq!(envelope["result"]["token"], json!(7), "{argv:?}");
        assert_eq!(envelope["command"], json!(command));
    }
}

#[tokio::test]
async fn a_secret_flag_is_kept_out_of_the_handlers_parse_errors() {
    for argv in [
        &["passwarden", "login", "ann", "--token=hunter2"][..],
        &["passwarden", "login", "ann", "--token", "hunter2"][..],
        &["passwarden", "login", "ann", "-t", "hunter2"][..],
    ] {
        let (_, envelope) = run(argv).await;
        assert_eq!(envelope["error"]["code"], json!("INVALID_FLAG"), "{argv:?}");
        assert!(
            envelope["error"]["message"]
                .as_str()
                .unwrap()
                .contains("<redacted>"),
            "{envelope}"
        );
    }
}

#[tokio::test]
async fn a_secret_positional_is_masked_and_kept_out_of_error_text() {
    let (_, envelope) = run(&["passwarden", "item", "set", "pin", "1234"]).await;
    assert_eq!(envelope["result"]["value"], json!(1234));
    assert_eq!(
        envelope["command"],
        json!("passwarden item set pin <redacted>")
    );

    let (_, envelope) = run(&["passwarden", "item", "set", "pin", "hunter2"]).await;
    assert_eq!(envelope["error"]["code"], json!("INVALID_ARG"));
    assert_eq!(
        envelope["command"],
        json!("passwarden item set pin <redacted>")
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("<redacted>")
    );

    // A secret positional named in a stream command, on the result line.
    let (execution, envelope) = run(&["passwarden", "watch", "hunter2"]).await;
    assert!(execution.is_stream());
    assert_eq!(envelope["command"], json!("passwarden watch <redacted>"));
}

#[tokio::test]
async fn the_hook_rewrites_arguments_it_recognizes() {
    let (_, envelope) = run(&["passwarden", "item", "edit", "github", "password=hunter2"]).await;
    // The handler received the real value.
    assert_eq!(
        envelope["result"]["assignment_len"],
        json!("password=hunter2".len())
    );
    assert_eq!(
        envelope["command"],
        json!("passwarden item edit github 'password=<redacted>'")
    );
}

#[tokio::test]
async fn the_hook_is_applied_to_every_error_that_echoes_an_argument() {
    // EXTRA_ARG quotes the surplus argument.
    let (_, envelope) = run(&["passwarden", "item", "fail", "x", "password=hunter2"]).await;
    assert_eq!(envelope["error"]["code"], json!("EXTRA_ARG"));
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("<redacted>")
    );
    // UNKNOWN_COMMAND and UNKNOWN_SUBCOMMAND quote the bad word.
    let (_, envelope) = run(&["passwarden", "password=hunter2"]).await;
    assert_eq!(envelope["error"]["code"], json!("UNKNOWN_COMMAND"));
    let (_, envelope) = run(&["passwarden", "store", "password=hunter2"]).await;
    assert_eq!(envelope["error"]["code"], json!("UNKNOWN_SUBCOMMAND"));
    let (_, envelope) = run(&["passwarden", "help", "password=hunter2"]).await;
    assert_eq!(envelope["error"]["code"], json!("UNKNOWN_COMMAND"));
    let (_, envelope) = run(&["passwarden", "version", "password=hunter2"]).await;
    assert_eq!(envelope["error"]["code"], json!("EXTRA_ARG"));
    // The hook also sees a flag's value.
    let (_, envelope) = run(&["passwarden", "item", "fail", "x", "--note=password=hunter2"]).await;
    assert_eq!(envelope["error"]["code"], json!("UNKNOWN_FLAG"));
}

#[tokio::test]
async fn every_gate_and_handler_error_that_echoes_argv_is_masked() {
    for argv in [
        // DRY_RUN_UNSUPPORTED
        vec![
            "passwarden",
            "item",
            "fail",
            "x",
            "--dry-run",
            "--k=hunter2",
        ],
        vec!["passwarden", "items", "--dry-run", "--k", "hunter2"],
        // INVALID_FLAG (a reserved boolean given a non-boolean)
        vec!["passwarden", "items", "--quiet=maybe", "--k=hunter2"],
        // A handler error
        vec!["passwarden", "item", "fail", "--k=hunter2", "x"],
        // MISSING_ARG, from the handler
        vec!["passwarden", "item", "edit", "--k=hunter2", "x"],
        // The skill built-in and the root
        vec!["passwarden", "--k=hunter2"],
        vec!["passwarden", "--k", "hunter2", "--select=commands"],
    ] {
        let (_, envelope) = run(&argv).await;
        assert!(
            envelope["command"].as_str().unwrap().contains("--k"),
            "{argv:?}: {envelope}"
        );
    }
}

#[tokio::test]
async fn a_parse_error_masks_the_value_it_quotes_and_the_whole_line() {
    for argv in [
        vec!["passwarden", "--=hunter2"],
        vec!["passwarden", "-ab=hunter2"],
        vec!["passwarden", "item", "set", "pin", "hunter2", "-ab=hunter2"],
        vec!["passwarden", "--k=hunter2", "--k=hunter2"],
        vec!["passwarden", "--k", "hunter2", "--k=hunter2"],
    ] {
        let (_, envelope) = run(&argv).await;
        assert_eq!(envelope["error"]["code"], json!("PARSE_ERROR"), "{argv:?}");
    }
    let (_, envelope) = run(&["passwarden", "item", "set", "pin", "hunter2", "-ab=hunter2"]).await;
    assert_eq!(
        envelope["command"],
        json!("passwarden item set <redacted> <redacted> -ab=<redacted>")
    );
}

#[tokio::test]
async fn the_select_advertisement_masks_the_command_it_reruns() {
    let (_, envelope) = run(&["passwarden", "items", "--limit=3", "--k", "hunter2"]).await;
    // `--k` is unknown, so the run is refused, with the line masked.
    assert_eq!(envelope["error"]["code"], json!("UNKNOWN_FLAG"));

    let cli = AgentCli::new("passwarden", "Password store")
        .clock(|| 0)
        .command(
            Command::new("items", "List items")
                .usage("passwarden items")
                .allow_unknown_flags()
                .handler(|_req| async move {
                    Ok(CommandOutput::list(vec![json!({ "id": 1, "name": "a" })]))
                }),
        );
    let run = cli.run(["passwarden", "items", "--k", "hunter2"]).await;
    let text = run.to_json();
    assert!(text.contains("--select=<fields>"), "{text}");
    assert!(text.contains("passwarden items --k <redacted>"), "{text}");
    assert!(!text.contains(SECRET), "{text}");
}

#[tokio::test]
async fn a_raw_commands_flag_values_are_masked_in_its_envelope() {
    let execution = cli()
        .run([
            "passwarden",
            "grep",
            "--key=hunter2",
            "-k",
            "hunter2",
            "pat",
        ])
        .await;
    assert!(execution.is_raw());
    let envelope = parse(&execution);
    assert_eq!(
        envelope["command"],
        json!("passwarden grep --key=<redacted> -k <redacted> pat")
    );

    // A prefix error names the whole line, tail included.
    let (_, envelope) = run(&["passwarden", "--bogus=hunter2", "grep", "--key=hunter2"]).await;
    assert_eq!(envelope["error"]["code"], json!("UNKNOWN_FLAG"));
    assert_eq!(
        envelope["command"],
        json!("passwarden --bogus=<redacted> grep --key=<redacted>")
    );

    // The hook reaches a raw command's positionals.
    let execution = cli().run(["passwarden", "grep", "password=hunter2"]).await;
    assert!(!printed(&execution).contains(SECRET));
}

#[tokio::test]
async fn the_handler_receives_the_real_values() {
    let (_, envelope) = run(&["passwarden", "login", "ann", "--token=5", "-t", "6"]).await;
    assert_eq!(envelope["result"]["token"], json!(11));
}

#[test]
fn audit_flags_a_secret_the_usage_does_not_declare() {
    let cli = AgentCli::new("app", "x")
        .command(
            Command::new("login", "Log in")
                .usage("app login <user> [--token=<value>]")
                .secret("tokne")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
        )
        .command(
            Command::new("fine", "Fine")
                .usage("app fine <user> [--token=<value>]")
                .secret("token")
                .secret("user")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
        )
        .command(
            Command::new("raw", "Raw")
                .usage("app raw [args...]")
                .secret("args")
                .raw_handler(|_args| async move { ExitCode::SUCCESS }),
        );
    let report = cli.audit();
    let codes: Vec<(&str, &str)> = report
        .findings
        .iter()
        .map(|finding| (finding.code.as_str(), finding.command.as_str()))
        .collect();
    assert!(codes.contains(&("UNDECLARED_SECRET", "login")), "{codes:?}");
    assert!(!codes.contains(&("UNDECLARED_SECRET", "fine")), "{codes:?}");
    assert!(
        codes.contains(&("SECRET_ON_RAW_COMMAND", "raw")),
        "{codes:?}"
    );
    assert!(!report.is_clean());
}
