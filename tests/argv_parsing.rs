//! How argv becomes an invocation, seen from the envelope: boolean flags,
//! malformed and repeated flags, the framework's own `--help`/`--version`,
//! unknown flags on every path, and the `command` field.

use agcli::{AgentCli, Command, CommandOutput, Execution, ExitCode};
use serde_json::{Value, json};

fn parse(execution: &Execution) -> Value {
    serde_json::from_str(&execution.to_json()).expect("envelope is JSON")
}

/// A calc-shaped CLI: `add <a> <b>`, and `note <text> [--tag=<tag>]`.
fn cli() -> AgentCli {
    AgentCli::new("calc", "Calculator")
        .version("1.0.0")
        .clock(|| 0)
        .command(
            Command::new("add", "Add two numbers")
                .usage("calc add <a> <b>")
                .handler(|req| async move {
                    let a: f64 = req.arg_parse(0, "a")?;
                    let b: f64 = req.arg_parse(1, "b")?;
                    Ok(CommandOutput::new(json!({ "result": a + b })))
                }),
        )
        .command(
            Command::new("note", "Store a note")
                .usage("calc note <text> [--tag=<tag>]")
                .handler(|req| {
                    let text = req.arg(0).unwrap_or_default().to_string();
                    async move { Ok(CommandOutput::new(json!({ "text": text }))) }
                }),
        )
}

/// The `error.code` of `execution`, asserting it failed with a usage error.
fn usage_error_code(execution: &Execution) -> String {
    let envelope = parse(execution);
    assert_eq!(envelope["ok"], json!(false), "{envelope}");
    assert_eq!(execution.exit_code(), ExitCode::USAGE, "{envelope}");
    envelope["error"]["code"]
        .as_str()
        .expect("error.code")
        .to_string()
}

#[tokio::test]
async fn a_short_bracketed_bool_flag_does_not_take_the_next_token() {
    let cli = AgentCli::new("app", "test").command(
        Command::new("run", "Run something")
            // `[-v]`: the shortest bracketed boolean the usage parser reads.
            .usage("app run [-v] <file>")
            .handler(|req| {
                let verbose = req.flag_on("v");
                let file = req.arg(0).unwrap_or("").to_string();
                async move {
                    Ok(CommandOutput::new(json!({
                        "verbose": verbose?,
                        "file": file,
                    })))
                }
            }),
    );

    let run = cli.run(["app", "run", "-v", "data.txt"]).await;
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

#[tokio::test]
async fn version_flags_read_their_value_strictly() {
    let version = json!({ "name": "calc", "version": "1.0.0" });
    for argv in [
        &["calc", "--version"][..],
        &["calc", "-V"],
        &["calc", "--version=true"],
        &["calc", "-V=yes"],
    ] {
        let envelope = parse(&cli().run(argv.iter().copied()).await);
        assert_eq!(envelope["result"], version, "{argv:?}: {envelope}");
    }

    // Spelled off, `--version` is absent: the root tree answers.
    for argv in [&["calc", "--version=false"][..], &["calc", "-V=0"]] {
        let envelope = parse(&cli().run(argv.iter().copied()).await);
        assert!(
            envelope["result"]["commands"].is_array(),
            "{argv:?}: {envelope}"
        );
    }

    // `-V` is a boolean, so it never swallows the next token: `note` stays a
    // positional, so `--tag` is checked against `note`'s schema, not the
    // root's (which would refuse it as UNKNOWN_FLAG).
    let envelope = parse(&cli().run(["calc", "-V", "note", "--tag=x"]).await);
    assert_eq!(envelope["result"], version, "{envelope}");

    let run = cli().run(["calc", "--version=maybe"]).await;
    assert_eq!(usage_error_code(&run), "INVALID_FLAG");
}

#[tokio::test]
async fn version_flags_answer_wherever_they_appear() {
    let version = json!({ "name": "calc", "version": "1.0.0" });
    for argv in [
        &["calc", "add", "1", "2", "--version"][..],
        &["calc", "add", "-V", "1", "2"],
        &["calc", "note", "hi", "--tag=x", "--version"],
    ] {
        let run = cli().run(argv.iter().copied()).await;
        assert_eq!(run.exit_code(), ExitCode::SUCCESS, "{argv:?}");
        let envelope = parse(&run);
        assert_eq!(envelope["result"], version, "{argv:?}: {envelope}");
    }

    // A flag the named command does not declare is still refused.
    let run = cli()
        .run(["calc", "add", "1", "2", "--bogus", "--version"])
        .await;
    assert_eq!(usage_error_code(&run), "UNKNOWN_FLAG");

    // Help wins when both are given.
    let envelope = parse(&cli().run(["calc", "add", "--version", "--help"]).await);
    assert_eq!(
        envelope["result"]["usage"],
        json!("calc add <a> <b>"),
        "{envelope}"
    );
}

#[tokio::test]
async fn a_version_flag_beside_an_unknown_command_names_the_command() {
    // `calc --version nope` used to answer the version and drop `nope`. As
    // with `--help`, a first positional that names no command is the error.
    for argv in [&["calc", "--version", "nope"][..], &["calc", "nope", "-V"]] {
        let run = cli().run(argv.iter().copied()).await;
        assert_eq!(usage_error_code(&run), "UNKNOWN_COMMAND", "{argv:?}");
        assert_eq!(
            parse(&run)["error"]["message"],
            json!("unknown command: nope"),
            "{argv:?}"
        );
    }
}

#[tokio::test]
async fn the_version_alias_takes_no_argument() {
    // `calc version extra` used to be `UNKNOWN_COMMAND: version`, pointing at
    // the wrong token.
    let run = cli().run(["calc", "version", "extra"]).await;
    assert_eq!(usage_error_code(&run), "EXTRA_ARG");
    let envelope = parse(&run);
    assert_eq!(
        envelope["error"]["message"],
        json!(
            "unexpected extra argument(s): \"extra\" (`version` takes 0 positional argument(s); got 1)"
        )
    );
    assert!(
        envelope["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("`calc version`")),
        "{envelope}"
    );
}

#[tokio::test]
async fn a_group_without_a_usage_string_refuses_unknown_flags() {
    // A group never runs, so it has no handler to leave an undeclared flag
    // to: `app db --bogus` used to answer the group's tree, flag dropped.
    let cli = AgentCli::new("app", "Grouped").clock(|| 0).command(
        Command::new("db", "Database commands").subcommand(
            Command::new("migrate", "Run migrations")
                .usage("app db migrate")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
        ),
    );
    for argv in [
        &["app", "db", "--bogus"][..],
        &["app", "help", "db", "--bogus"],
        &["app", "db", "--help", "--bogus"],
    ] {
        let run = cli.run(argv.iter().copied()).await;
        assert_eq!(usage_error_code(&run), "UNKNOWN_FLAG", "{argv:?}");
        assert!(
            parse(&run)["fix"]
                .as_str()
                .is_some_and(|fix| fix.starts_with("`db` takes no flags of its own.")),
            "{argv:?}: {}",
            run.to_json()
        );
    }
    // The reserved flags still pass, and the tree still answers.
    let envelope = parse(&cli.run(["app", "db", "--quiet"]).await);
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(
        envelope["result"]["subcommands"][0]["name"],
        json!("migrate")
    );
}

#[tokio::test]
async fn reserved_booleans_accept_only_boolean_spellings() {
    // Off spellings, in any case, keep the next actions.
    for value in ["false", "0", "no", "off", "OFF"] {
        let flag = format!("--quiet={value}");
        let envelope = parse(&cli().run(["calc", "add", "1", "2", flag.as_str()]).await);
        assert_eq!(envelope["ok"], json!(true), "{flag}: {envelope}");
        assert_ne!(envelope["next_actions"], json!([]), "{flag}: {envelope}");
    }
    for value in ["true", "1", "yes", "on", "Yes"] {
        let flag = format!("--quiet={value}");
        let envelope = parse(&cli().run(["calc", "add", "1", "2", flag.as_str()]).await);
        assert_eq!(envelope["next_actions"], json!([]), "{flag}: {envelope}");
    }

    // Anything else is refused, on a command and on the root tree alike.
    for argv in [
        &["calc", "add", "1", "2", "--quiet=maybe"][..],
        &["calc", "--compact=2"],
        &["calc", "add", "1", "2", "--dry-run="],
        &["calc", "help", "add", "--json=sure"],
    ] {
        let run = cli().run(argv.iter().copied()).await;
        assert_eq!(usage_error_code(&run), "INVALID_FLAG", "{argv:?}");
    }
    let envelope = parse(&cli().run(["calc", "--quiet=maybe"]).await);
    assert_eq!(
        envelope["error"]["message"],
        json!(r#"flag --quiet is not a boolean: "maybe""#)
    );
    assert!(
        envelope["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("true/false")),
        "{envelope}"
    );
}

#[tokio::test]
async fn a_non_boolean_on_a_command_offers_that_commands_templates() {
    let run = cli().run(["calc", "add", "1", "2", "--quiet=maybe"]).await;
    assert_eq!(usage_error_code(&run), "INVALID_FLAG");
    let commands: Vec<Value> = parse(&run)["next_actions"]
        .as_array()
        .expect("next_actions")
        .iter()
        .map(|action| action["command"].clone())
        .collect();
    assert_eq!(commands, [json!("calc add <a> <b>"), json!("calc")]);
}

#[tokio::test]
async fn a_non_boolean_naming_no_command_offers_the_root_actions() {
    let root_actions = parse(&cli().run(["calc"]).await)["next_actions"].clone();
    for argv in [
        &["calc", "--quiet=maybe"][..],
        &["calc", "bogus", "--help=maybe"],
    ] {
        let run = cli().run(argv.iter().copied()).await;
        assert_eq!(usage_error_code(&run), "INVALID_FLAG", "{argv:?}");
        assert_eq!(parse(&run)["next_actions"], root_actions, "{argv:?}");
    }
}

#[tokio::test]
async fn bundled_short_flags_with_a_value_are_a_parse_error() {
    let run = cli().run(["calc", "add", "1", "2", "-abc=5"]).await;
    assert_eq!(usage_error_code(&run), "PARSE_ERROR");
    assert_eq!(
        parse(&run)["error"]["message"],
        json!("invalid flag syntax: -abc=5")
    );
    let envelope = parse(&run);
    let fix = envelope["fix"].as_str().expect("fix");
    assert!(fix.contains("`--name=value`"), "{fix}");
    assert!(
        fix.contains("Bundled short flags (`-abc`) take no value"),
        "{fix}"
    );
}

#[tokio::test]
async fn a_flag_given_twice_is_a_parse_error_naming_it() {
    let run = cli()
        .run(["calc", "add", "1", "2", "--select=result", "--select=a"])
        .await;
    assert_eq!(usage_error_code(&run), "PARSE_ERROR");
    assert_eq!(
        parse(&run)["error"]["message"],
        json!("--select given twice")
    );
    assert_eq!(
        parse(&run)["fix"],
        json!(
            "Pass --select once, with every field in one comma-separated list \
             (`--select=id,name`)."
        )
    );

    let run = cli()
        .run(["calc", "note", "x", "--tag", "a", "--tag=b"])
        .await;
    assert_eq!(parse(&run)["error"]["message"], json!("--tag given twice"));
    assert_eq!(parse(&run)["fix"], json!("Pass --tag once."));
}

#[tokio::test]
async fn unknown_flags_are_rejected_on_the_root_help_and_version_paths() {
    for argv in [
        &["calc", "--bogus"][..],
        &["calc", "--version", "--bogus"],
        &["calc", "version", "--bogus"],
        &["calc", "help", "--bogus"],
        &["calc", "--help", "--bogus"],
    ] {
        let run = cli().run(argv.iter().copied()).await;
        assert_eq!(usage_error_code(&run), "UNKNOWN_FLAG", "{argv:?}");
        assert!(
            parse(&run)["fix"]
                .as_str()
                .is_some_and(|fix| fix.starts_with("`calc` takes no flags of its own.")),
            "{argv:?}: {}",
            run.to_json()
        );
    }

    // Help for a command checks the flags against that command's schema.
    let run = cli().run(["calc", "help", "note", "--tga=x"]).await;
    assert_eq!(usage_error_code(&run), "UNKNOWN_FLAG");
    let fix = parse(&run)["fix"].as_str().expect("fix").to_string();
    assert!(fix.starts_with("Did you mean `--tag`?"), "{fix}");
    assert!(fix.contains("Valid flags for `note`: --tag."), "{fix}");

    // Declared and reserved flags still pass on every path.
    for argv in [
        &["calc", "help", "note", "--tag=x"][..],
        &["calc", "note", "--help", "--tag=x"],
        &["calc", "--select=commands"],
        &["calc", "--version", "--quiet"],
    ] {
        let envelope = parse(&cli().run(argv.iter().copied()).await);
        assert_eq!(envelope["ok"], json!(true), "{argv:?}: {envelope}");
    }
}

#[tokio::test]
async fn the_command_field_reruns_as_printed() {
    let run = cli()
        .run(["/usr/local/bin/calc", "note", "two words", "--tag=it's"])
        .await;
    let envelope = parse(&run);
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(
        envelope["command"],
        json!(r"calc note 'two words' '--tag=it'\''s'")
    );

    // A handler error and a gate error render the same way.
    let run = cli().run(["calc", "add", "one two", "2"]).await;
    assert_eq!(parse(&run)["command"], json!("calc add 'one two' 2"));
    let run = cli().run(["calc", "add", "1", "2", "a b"]).await;
    assert_eq!(usage_error_code(&run), "EXTRA_ARG");
    assert_eq!(parse(&run)["command"], json!("calc add 1 2 'a b'"));
}

#[tokio::test]
async fn a_line_that_fails_to_parse_is_named_like_any_other() {
    // Before the fix this rendered the full argv[0] path and unquoted tokens.
    let run = cli()
        .run(["/usr/local/bin/calc", "--=x", "two words"])
        .await;
    assert_eq!(usage_error_code(&run), "PARSE_ERROR");
    assert_eq!(parse(&run)["command"], json!("calc --=x 'two words'"));
}

#[tokio::test]
async fn a_commands_name_is_never_a_flags_value() {
    // `--bogus` is given no value, so `add 1 2` is the command line, and the
    // unknown flag is answered for `add`.
    let run = cli().run(["calc", "--bogus", "add", "1", "2"]).await;
    assert_eq!(usage_error_code(&run), "UNKNOWN_FLAG");
    let envelope = parse(&run);
    assert_eq!(
        envelope["error"]["message"],
        json!("unknown flag(s): --bogus")
    );
    let fix = envelope["fix"].as_str().expect("fix");
    assert!(fix.starts_with("`add` takes no flags of its own."), "{fix}");

    // `add` is the command even though `note` declares `--tag`: the flag
    // lands on `add`, which takes none.
    let run = cli().run(["calc", "--tag", "add", "note", "x"]).await;
    assert_eq!(usage_error_code(&run), "UNKNOWN_FLAG");
    let envelope = parse(&run);
    assert_eq!(
        envelope["error"]["message"],
        json!("unknown flag(s): --tag")
    );
    let fix = envelope["fix"].as_str().expect("fix");
    assert!(fix.starts_with("`add` takes no flags of its own."), "{fix}");

    // `=` passes a value spelled like a command.
    let run = cli().run(["calc", "--tag=add", "note", "x"]).await;
    let envelope = parse(&run);
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(envelope["result"]["text"], json!("x"));
}
