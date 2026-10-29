//! Secrets given on the command line stay out of every envelope: the value of
//! a flag the usage does not declare, a `Command::secret` flag or positional,
//! and whatever the `AgentCli::redact_arg` hook rewrites. The handler still
//! reads the real values.

use std::str::FromStr;

use agcli::{AgentCli, Command, CommandError, CommandOutput, ErrorCode, Execution, ExitCode};
use serde_json::{Value, json};

const SECRET: &str = "hunter2";

/// A value whose parse error repeats its input.
struct Echo;

impl FromStr for Echo {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        Err(format!("cannot read {text}"))
    }
}

/// A password manager: `store unlock` declares no flags, `login` declares a
/// secret `--token`, `item set` takes a secret `<value>`, `item edit` takes a
/// `field=value` assignment the hook knows about, and `stream` streams.
fn cli() -> AgentCli {
    more_commands(base_cli())
}

fn base_cli() -> AgentCli {
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
                .usage("passwarden login <user> [--token=<value>] [-t <value>] [--format=<f>] [-v]")
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
                .stream_handler(|req, _out| async move {
                    let _: u32 = req.arg_parse(0, "name")?;
                    Ok(CommandOutput::new(json!({})))
                }),
        )
}

/// The rest of the CLI: the commands that probe one rule each.
fn more_commands(cli: AgentCli) -> AgentCli {
    cli.command(
        Command::new("echo", "Parse values that fail")
            .usage("passwarden echo <value> [--opt=<o>]")
            .secret("value")
            .secret("opt")
            .handler(|req| async move {
                req.flag_parse::<Echo>("opt")?;
                req.arg_parse::<Echo>(0, "value")?;
                Ok(CommandOutput::new(json!({})))
            }),
    )
    .command(
        Command::new("ship", "Optional then secret")
            .usage("passwarden ship <name> [<note>] [<pin>]")
            .secret("pin")
            .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
    )
    .command(
        Command::new("put", "Variadic secret")
            .usage("passwarden put <name> [values...]")
            .secret("values")
            .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
    )
    .command(
        Command::new("acct", "Account operations")
            .usage("passwarden acct [--token=<t>]")
            .secret("token")
            .subcommand(
                Command::new("show", "Show the account")
                    .usage("passwarden acct show")
                    .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
            ),
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

    // A secret positional of a stream command, on the terminal error line.
    let (execution, envelope) = run(&["passwarden", "watch", "hunter2"]).await;
    assert!(execution.is_stream());
    assert_eq!(envelope["command"], json!("passwarden watch <redacted>"));
    assert_eq!(envelope["error"]["code"], json!("INVALID_ARG"));
    // And on the result line.
    let (execution, envelope) = run(&["passwarden", "watch", "7"]).await;
    assert!(execution.is_stream());
    assert_eq!(envelope["command"], json!("passwarden watch <redacted>"));
    assert_eq!(envelope["ok"], json!(true));
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
        // The root
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

/// Assert none of the secret's letters is anywhere in what `execution` prints.
fn assert_no_dashed(execution: &Execution, argv: &[&str]) {
    let text = printed(execution);
    assert!(!text.contains("gbk"), "{argv:?} leaked: {text}");
    for letter in ["-g", "-k", "-9", "--g"] {
        assert!(!text.contains(letter), "{argv:?} leaked {letter}: {text}");
    }
}

#[tokio::test]
async fn a_value_that_starts_with_a_dash_is_masked_whole_and_never_listed_as_a_flag() {
    for tail in [
        &["--passphrase", "-gbk9"][..],
        &["--passphrase", "--gbk9"][..],
        &["-p", "-gbk9"][..],
        &["-p", "--gbk9"][..],
        &["--passphrase", "--gbk9=1"][..],
    ] {
        let mut argv = vec!["passwarden", "store", "unlock"];
        argv.extend_from_slice(tail);
        let execution = cli().run(argv.iter().copied()).await;
        assert_no_dashed(&execution, &argv);
        let envelope = parse(&execution);
        assert_eq!(envelope["error"]["code"], json!("UNKNOWN_FLAG"), "{argv:?}");
        let flag = if tail[0] == "-p" {
            "-p"
        } else {
            "--passphrase"
        };
        assert_eq!(
            envelope["error"]["message"],
            json!(format!("unknown flag(s): {flag}, <redacted>")),
            "{argv:?}"
        );
        assert_eq!(
            envelope["command"],
            json!(format!("passwarden store unlock {flag} <redacted>")),
            "{argv:?}"
        );
    }
    // The inline forms carry the value after the `=`.
    for tail in [&["--passphrase=-gbk9"][..], &["-p=-gbk9"][..]] {
        let mut argv = vec!["passwarden", "store", "unlock"];
        argv.extend_from_slice(tail);
        let execution = cli().run(argv.iter().copied()).await;
        assert_no_dashed(&execution, &argv);
        assert_eq!(parse(&execution)["error"]["code"], json!("UNKNOWN_FLAG"));
    }
    // A secret flag that is declared.
    for tail in [
        &["--token", "-gbk9"][..],
        &["-t", "--gbk9"][..],
        &["--token=-gbk9"][..],
    ] {
        let mut argv = vec!["passwarden", "login", "ann"];
        argv.extend_from_slice(tail);
        let execution = cli().run(argv.iter().copied()).await;
        assert_no_dashed(&execution, &argv);
    }
}

#[tokio::test]
async fn a_value_that_starts_with_a_dash_in_a_line_that_fails_to_parse() {
    for argv in [
        &["passwarden", "--passphrase", "--gbk9", "--gbk9"][..],
        &["passwarden", "--passphrase", "-gbk9=1", "-ab=2"][..],
        &["passwarden", "-p", "--=gbk9"][..],
        &["passwarden", "-p", "-gb=k9"][..],
    ] {
        let execution = cli().run(argv.iter().copied()).await;
        assert_no_dashed(&execution, argv);
        assert_eq!(parse(&execution)["error"]["code"], json!("PARSE_ERROR"));
    }
    // A raw command's tail is masked the same way.
    let argv = ["passwarden", "grep", "-c", "--gbk9", "pat"];
    let execution = cli().run(argv).await;
    assert_no_dashed(&execution, &argv);
}

#[tokio::test]
async fn a_secret_short_flag_in_a_bundle_masks_the_token_after_it() {
    let argv = ["passwarden", "login", "ann", "-vt", "gbk9"];
    let execution = cli().run(argv).await;
    assert_no_dashed(&execution, &argv);
    assert_eq!(
        parse(&execution)["command"],
        json!("passwarden login ann -vt <redacted>")
    );
}

#[tokio::test]
async fn a_parse_error_that_repeats_a_masked_value_is_left_out() {
    for argv in [
        &["passwarden", "echo", "hunter2"][..],
        &["passwarden", "echo", "x", "--opt=hunter2"][..],
        &["passwarden", "echo", "x", "--opt", "hunter2"][..],
    ] {
        let (_, envelope) = run(argv).await;
        let message = envelope["error"]["message"].as_str().unwrap();
        assert!(
            message.ends_with(r#": "<redacted>""#),
            "{argv:?}: {message}"
        );
        assert!(!message.contains("cannot read"), "{argv:?}: {message}");
    }
}

#[tokio::test]
async fn a_secret_after_an_optional_positional_masks_from_the_optional_on() {
    // Skipping the note puts the pin in the note's slot.
    let (_, envelope) = run(&["passwarden", "ship", "box", "hunter2"]).await;
    assert_eq!(envelope["command"], json!("passwarden ship box <redacted>"));
    // With every positional given, the note is a note.
    let (_, envelope) = run(&["passwarden", "ship", "box", "a note", "hunter2"]).await;
    assert_eq!(
        envelope["command"],
        json!("passwarden ship box 'a note' <redacted>")
    );
    // The name before the optional is never masked.
    let (_, envelope) = run(&["passwarden", "ship", "box"]).await;
    assert_eq!(envelope["command"], json!("passwarden ship box"));
}

#[tokio::test]
async fn a_variadic_secret_masks_from_its_position_on() {
    let (_, envelope) = run(&["passwarden", "put", "box", "hunter2", "hunter2"]).await;
    assert_eq!(
        envelope["command"],
        json!("passwarden put box <redacted> <redacted>")
    );
}

#[tokio::test]
async fn a_parents_secret_covers_its_subcommands() {
    let (_, envelope) = run(&["passwarden", "acct", "show", "--token=hunter2"]).await;
    assert_eq!(envelope["ok"], json!(true));
    assert_eq!(
        envelope["command"],
        json!("passwarden acct show --token=<redacted>")
    );
    // Before the subcommand's name too.
    let (_, envelope) = run(&["passwarden", "acct", "--token", "hunter2", "show"]).await;
    assert_eq!(
        envelope["command"],
        json!("passwarden acct --token <redacted> show")
    );
}

#[tokio::test]
async fn a_secret_flag_before_the_command_name_is_masked() {
    let (_, envelope) = run(&["passwarden", "--token", "hunter2", "login", "ann"]).await;
    assert_eq!(
        envelope["command"],
        json!("passwarden --token <redacted> login ann")
    );
}

#[tokio::test]
async fn flag_values_before_a_raw_commands_name_are_always_masked() {
    // `--key` is not the raw command's to take from there, declared or not.
    let cli = AgentCli::new("passwarden", "x").clock(|| 0).command(
        Command::new("grep", "Search")
            .usage("passwarden grep [--key=<k>] <pattern>")
            .raw_handler(|_args| async move { ExitCode::SUCCESS }),
    );
    let run = cli
        .run(["passwarden", "--key=hunter2", "grep", "pat"])
        .await;
    let text = run.to_json();
    assert!(!text.contains(SECRET), "{text}");
    assert!(text.contains("--key=<redacted>"), "{text}");
}

#[tokio::test]
async fn words_nothing_masks_keep_their_shell_quoting() {
    let (_, envelope) = run(&["passwarden", "login", "two words", "--format=a b"]).await;
    assert_eq!(
        envelope["command"],
        json!("passwarden login 'two words' '--format=a b'")
    );
    let (_, envelope) = run(&["passwarden", "item", "edit", "it's", "password=hunter2"]).await;
    assert_eq!(
        envelope["command"],
        json!(r"passwarden item edit 'it'\''s' 'password=<redacted>'")
    );
}

#[test]
fn audit_checks_a_secret_against_the_commands_path() {
    let cli = AgentCli::new("app", "x").command(
        Command::new("acct", "Account")
            .usage("app acct [--token=<t>]")
            .secret("token")
            .secret("key")
            .subcommand(
                Command::new("show", "Show")
                    .usage("app show [--key=<k>] <id> [<note>] [<pin>]")
                    .secret("token")
                    .secret("pin")
                    .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
            ),
    );
    let report = cli.audit();
    let codes: Vec<(&str, &str)> = report
        .findings
        .iter()
        .map(|finding| (finding.code.as_str(), finding.command.as_str()))
        .collect();
    // `key` is declared by a subcommand, `token` by the parent.
    assert!(
        !codes.iter().any(|(code, _)| *code == "UNDECLARED_SECRET"),
        "{codes:?}"
    );
    assert!(
        codes.contains(&("SECRET_AFTER_OPTIONAL", "acct show")),
        "{codes:?}"
    );
}
