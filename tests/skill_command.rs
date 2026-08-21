//! `AgentCli::skill` — help becomes a skill.
//!
//! The built-in `skill` command renders the live command tree as a `SKILL.md`
//! document, so an agent that can run the binary once can bootstrap itself
//! from it. These tests pin what that document has to carry (every command
//! template, the reserved flags, the exit and error dictionaries) and the
//! `--install` write path.

use agcli::{AgentCli, Command, CommandOutput, Envelope, ExitCode};
use serde_json::json;

/// A CLI with a flat command, a nested group, and the `skill` command last.
fn app() -> AgentCli {
    AgentCli::new("app", "Agent-native test CLI")
        .version("2.1.0")
        .command(
            Command::new("status", "Show system status")
                .usage("app status")
                .handler(|_req, _ctx| {
                    Box::pin(async move { Ok(CommandOutput::new(json!({ "healthy": true }))) })
                }),
        )
        .command(
            Command::new("db", "Database commands").subcommand(
                Command::new("migrate", "Run pending migrations")
                    .usage("app db migrate [--dry-run]")
                    .handler(|_req, _ctx| {
                        Box::pin(async move { Ok(CommandOutput::new(json!({ "applied": 0 }))) })
                    }),
            ),
        )
        .skill()
}

fn success(execution: &agcli::Execution) -> &agcli::SuccessEnvelope {
    match execution.envelope() {
        Envelope::Success(envelope) => envelope,
        Envelope::Error(envelope) => panic!("expected success envelope, got {:?}", envelope.error),
    }
}

/// A unique scratch directory under the system temp dir. No `tempfile`
/// dependency: agcli ships none and the skill feature needs none.
fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("agcli-skill-{tag}-{unique}"));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn skill_markdown_documents_the_whole_tree() {
    let markdown = app().skill_markdown();

    // Frontmatter an agent harness can load.
    assert!(markdown.starts_with("---\nname: app\n"), "{markdown}");
    assert!(markdown.contains("description: Agent-native test CLI."));
    assert!(markdown.contains("Version 2.1.0."));

    // Every command template, nested ones included.
    assert!(markdown.contains("`app status`"), "{markdown}");
    assert!(markdown.contains("`app db <subcommand>`"), "{markdown}");
    assert!(
        markdown.contains("`app db migrate [--dry-run]`"),
        "{markdown}"
    );
    assert!(
        markdown.contains("`app skill [--install=<dir>]`"),
        "{markdown}"
    );

    // The dictionaries the root tree publishes.
    assert!(markdown.contains("`7`"), "{markdown}");
    assert!(markdown.contains("UNKNOWN_COMMAND"), "{markdown}");

    // The reserved agent flags, on by default.
    assert!(markdown.contains("--select"), "{markdown}");
}

#[test]
fn reserved_flags_off_drops_the_agent_flag_section() {
    let markdown = AgentCli::new("app", "Test CLI")
        .reserved_flags(false)
        .skill()
        .skill_markdown();
    assert!(!markdown.contains("--select"), "{markdown}");
    assert!(markdown.contains("## Exit codes"), "{markdown}");
}

#[test]
fn a_raw_command_is_marked_as_raw() {
    let markdown = AgentCli::new("app", "Test CLI")
        .command(
            Command::new("grep", "Search files")
                .usage("app grep [args...]")
                .raw_handler(|_argv, _ctx| Box::pin(async move { 0 })),
        )
        .skill()
        .skill_markdown();
    assert!(
        markdown.contains("(raw: prints the tool's own output, not an envelope)"),
        "{markdown}"
    );
}

#[tokio::test]
async fn skill_command_returns_the_markdown() {
    let run = app().run_argv(["app", "skill"]).await;
    assert_eq!(run.exit_code(), ExitCode::SUCCESS);
    let envelope = success(&run);
    assert_eq!(envelope.result["skill_name"], json!("app"));
    let markdown = envelope.result["markdown"]
        .as_str()
        .expect("markdown is a string");
    assert!(markdown.starts_with("---\nname: app\n"), "{markdown}");
    assert!(!envelope.next_actions.is_empty());
}

#[tokio::test]
async fn install_writes_the_skill_file() {
    let dir = scratch_dir("install");
    let flag = format!("--install={}", dir.display());
    let run = app().run_argv(["app", "skill", &flag]).await;
    assert_eq!(run.exit_code(), ExitCode::SUCCESS);

    let envelope = success(&run);
    let written = envelope.result["path"].as_str().expect("path is a string");
    let expected = std::path::absolute(dir.join("app").join("SKILL.md")).expect("absolute path");
    assert_eq!(written, expected.display().to_string());
    assert!(std::path::Path::new(written).is_absolute(), "{written}");

    // The file is the answer, so the markdown does not ride along too.
    assert!(
        envelope.result.get("markdown").is_none(),
        "{:?}",
        envelope.result
    );

    let on_disk = std::fs::read_to_string(&expected).expect("SKILL.md was written");
    assert_eq!(on_disk, app().skill_markdown());
    assert_eq!(envelope.result["bytes"], json!(on_disk.len()));

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn install_under_dry_run_reports_the_path_and_writes_nothing() {
    let dir = scratch_dir("dryrun");
    let flag = format!("--install={}", dir.display());
    let run = app().run_argv(["app", "skill", &flag, "--dry-run"]).await;
    assert_eq!(run.exit_code(), ExitCode::SUCCESS);

    let envelope = success(&run);
    assert_eq!(envelope.result["dry_run"], json!(true));
    let written = envelope.result["path"].as_str().expect("path is a string");
    assert!(
        !std::path::Path::new(written).exists(),
        "{written} was written anyway"
    );
    assert!(
        !dir.join("app").exists(),
        "the skill directory was created anyway"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_bare_install_flag_is_rejected() {
    // `--install` with no value parses as the boolean sentinel "true"; writing
    // ./true/app/SKILL.md and reporting success would be a wrong answer.
    let run = app().run_argv(["app", "skill", "--install"]).await;
    let Envelope::Error(envelope) = run.envelope() else {
        panic!("expected an error envelope");
    };
    assert_eq!(envelope.error.code, "INVALID_FLAG");
    assert!(
        !std::path::Path::new("true").exists(),
        "wrote a `true` directory"
    );
}

#[tokio::test]
async fn an_unexpanded_tilde_is_rejected() {
    // A shell expands `~` only at the start of a word, never after `=`.
    let run = app().run_argv(["app", "skill", "--install=~/skills"]).await;
    let Envelope::Error(envelope) = run.envelope() else {
        panic!("expected an error envelope");
    };
    assert_eq!(envelope.error.code, "INVALID_FLAG");
    assert!(envelope.fix.contains("absolute path"), "{}", envelope.fix);
}

/// The framework dispatches `skill` after the unknown-flag gate, which moved
/// the handler-less `MISSING_HANDLER` check below it. A typo'd flag must still
/// be the reported failure.
#[tokio::test]
async fn an_unknown_flag_beats_a_missing_handler() {
    let cli = AgentCli::new("app", "Test CLI")
        .command(Command::new("orphan", "No handler").usage("app orphan [--real]"));
    let run = cli.run_argv(["app", "orphan", "--bogus"]).await;
    let Envelope::Error(envelope) = run.envelope() else {
        panic!("expected an error envelope");
    };
    assert_eq!(envelope.error.code, "UNKNOWN_FLAG");
}

#[test]
fn audit_rejects_a_cli_name_that_is_not_a_slug() {
    let report = AgentCli::new("My App", "Test CLI").skill().audit();
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.code == "SKILL_NAME_INVALID"),
        "{:?}",
        report.findings
    );
    // Without the skill command the name is nobody's business.
    assert!(
        !AgentCli::new("My App", "Test CLI")
            .audit()
            .findings
            .iter()
            .any(|f| f.code == "SKILL_NAME_INVALID")
    );
    // A slug name is clean.
    assert!(app().audit().is_clean(), "{:?}", app().audit().findings);
}

#[tokio::test]
async fn install_into_an_unwritable_path_reports_write_failed() {
    let dir = scratch_dir("unwritable");
    // A regular file where the skill directory has to go: `create_dir_all`
    // cannot win, and the envelope has to say so instead of panicking.
    let blocker = dir.join("app");
    std::fs::write(&blocker, b"not a directory").expect("write blocker file");

    let flag = format!("--install={}", dir.display());
    let run = app().run_argv(["app", "skill", &flag]).await;
    let Envelope::Error(envelope) = run.envelope() else {
        panic!("expected an error envelope");
    };
    assert_eq!(envelope.error.code, "WRITE_FAILED");
    assert_eq!(run.exit_code(), ExitCode::ERROR);
    assert_eq!(envelope.fix, "Check the directory exists and is writable.");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn root_tree_lists_skill() {
    let run = app().run_argv(["app"]).await;
    let commands = success(&run).result["commands"]
        .as_array()
        .expect("commands array");
    assert!(
        commands.iter().any(|c| c["name"] == json!("skill")),
        "{commands:?}"
    );
}

#[tokio::test]
async fn skill_honors_select() {
    let run = app()
        .run_argv(["app", "skill", "--select=skill_name"])
        .await;
    assert_eq!(success(&run).result, json!({ "skill_name": "app" }));
}
