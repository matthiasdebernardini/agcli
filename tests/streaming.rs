//! Streaming commands (`Command::stream_handler`) under in-process `run()`.
//!
//! The handler writes events; the framework writes exactly one terminal line,
//! the envelope with a `type`. In-process the events are captured in the
//! `Execution` and nothing reaches the real stdout. The live-stdout half is in
//! `tests/ops_example.rs` (and, for a panic, `tests/panic_hook.rs`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use agcli::{
    AgentCli, Command, CommandError, CommandOutput, ErrorCode, ExitCode, LogLevel, NextAction,
    StepStatus, StreamEvent, StreamWriteError, StreamWriter,
};
use serde_json::{Value, json};

/// A CLI whose `deploy <service>` streams two steps and a log line, and whose
/// `fail` streams one step and then returns an error.
fn stream_cli() -> AgentCli {
    AgentCli::new("app", "Streaming test CLI")
        .command(
            Command::new("deploy", "Deploy, streaming each step")
                .usage("app deploy <service>")
                .stream_handler(|req, out| async move {
                    let service = req.require_arg(0, "service")?.to_string();
                    for step in ["build", "ship"] {
                        out.emit(&StreamEvent::Step {
                            name: step.to_string(),
                            status: StepStatus::Completed,
                            duration_ms: Some(1),
                            error: None,
                        })?;
                    }
                    out.emit(&StreamEvent::Log {
                        level: LogLevel::Info,
                        message: format!("{service} is live"),
                    })?;
                    Ok(
                        CommandOutput::new(json!({ "service": service, "deployed": true }))
                            .next_action(NextAction::new("app status", "Check health")),
                    )
                }),
        )
        .command(
            Command::new("fail", "Stream, then fail")
                .usage("app fail")
                .stream_handler(|_req, out| async move {
                    out.emit(&StreamEvent::Step {
                        name: "build".to_string(),
                        status: StepStatus::Failed,
                        duration_ms: None,
                        error: Some("compiler exploded".to_string()),
                    })?;
                    Err(CommandError::new(
                        ErrorCode::new("BUILD_FAILED"),
                        "build failed",
                        "Fix the build.",
                    ))
                }),
        )
}

fn parse(line: &str) -> Value {
    serde_json::from_str(line).expect("each line is JSON")
}

#[tokio::test]
async fn events_are_captured_and_the_terminal_line_comes_last() {
    let run = stream_cli().run(["app", "deploy", "web"]).await;

    assert!(run.is_stream());
    let events: Vec<Value> = run.stream_events().iter().map(|line| parse(line)).collect();
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0]["type"], json!("step"));
    assert_eq!(events[0]["name"], json!("build"));
    assert_eq!(events[2]["type"], json!("log"));
    for event in &events {
        assert!(
            event.get("ok").is_none(),
            "events are not terminal: {event}"
        );
        assert!(event["ts"].as_str().is_some_and(|ts| ts.ends_with('Z')));
    }

    let terminal = parse(&run.to_json());
    assert_eq!(terminal["type"], json!("result"));
    assert_eq!(terminal["ok"], json!(true));
    assert_eq!(terminal["result"]["service"], json!("web"));
    assert_eq!(terminal["next_actions"][0]["command"], json!("app status"));
    assert_eq!(run.exit_code(), ExitCode::SUCCESS);
    // The key order matches a plain envelope, with `type` in front.
    assert!(
        run.to_json()
            .starts_with(r#"{"type":"result","ok":true,"command":"app deploy web""#)
    );
}

#[tokio::test]
async fn a_failing_stream_handler_ends_with_a_typed_error_line() {
    let run = stream_cli().run(["app", "fail"]).await;

    assert_eq!(run.stream_events().len(), 1);
    assert_eq!(parse(&run.stream_events()[0])["status"], json!("failed"));
    let terminal = parse(&run.to_json());
    assert_eq!(terminal["type"], json!("error"));
    assert_eq!(terminal["ok"], json!(false));
    assert_eq!(terminal["error"]["code"], json!("BUILD_FAILED"));
    assert_eq!(terminal["fix"], json!("Fix the build."));
    assert_eq!(run.exit_code(), ExitCode::ERROR);
}

#[tokio::test]
async fn a_non_boolean_flag_answers_with_the_streams_terminal_error_line() {
    let run = stream_cli()
        .run(["app", "deploy", "web", "--quiet=maybe"])
        .await;

    assert!(run.is_stream());
    assert_eq!(run.stream_events(), [] as [String; 0]);
    let terminal = parse(&run.to_json());
    assert_eq!(terminal["type"], json!("error"));
    assert_eq!(terminal["error"]["code"], json!("INVALID_FLAG"));
    assert_eq!(
        terminal["next_actions"][0]["command"],
        json!("app deploy <service>")
    );
    assert_eq!(run.exit_code(), ExitCode::USAGE);
}

#[tokio::test]
async fn a_terminal_error_line_carries_the_structured_payload() {
    // The same failure reads the same on both channels: the terminal line is
    // the error envelope, `data` included, between `fix` and `next_actions`.
    let cli = AgentCli::new("app", "x").schema_version("v2").command(
        Command::new("post", "Post, then reject some")
            .usage("app post")
            .stream_handler(|_req, _out| async move {
                Err(CommandError::new(
                    ErrorCode::new("PARTIAL"),
                    "2 of 3 rejected",
                    "Fix the rejected rows.",
                )
                .data(json!({ "rejected": [{ "line": 91 }] })))
            }),
    );
    let run = cli.run(["app", "post"]).await;
    let line = run.to_json();
    let terminal = parse(&line);
    assert_eq!(terminal["type"], json!("error"));
    assert_eq!(terminal["schema_version"], json!("v2"));
    assert_eq!(terminal["data"]["rejected"][0]["line"], json!(91));
    let fix_at = line.find("\"fix\"").expect("fix key");
    let data_at = line.find("\"data\"").expect("data key");
    let actions_at = line.find("\"next_actions\"").expect("next_actions key");
    assert!(fix_at < data_at && data_at < actions_at, "{line}");
}

#[tokio::test]
async fn reserved_flags_apply_to_the_terminal_line() {
    let run = stream_cli()
        .run(["app", "deploy", "web", "--select=service", "--quiet"])
        .await;

    assert_eq!(run.stream_events().len(), 3, "events are not projected");
    let terminal = parse(&run.to_json());
    assert_eq!(terminal["type"], json!("result"));
    assert_eq!(terminal["result"], json!({ "service": "web" }));
    assert_eq!(terminal["next_actions"], json!([]));
}

#[tokio::test]
async fn a_rejected_invocation_of_a_streaming_command_is_still_a_terminal_line() {
    // A stream consumer reads `type` on every line, so even a usage error
    // that stops the handler from running answers as a terminal line.
    for argv in [
        vec!["app", "deploy", "web", "--bogus"],
        vec!["app", "deploy", "web", "extra"],
        vec!["app", "deploy", "web", "--dry-run"],
    ] {
        let run = stream_cli().run(argv.clone()).await;
        assert!(run.is_stream(), "{argv:?}");
        assert!(run.stream_events().is_empty(), "{argv:?}");
        let terminal = parse(&run.to_json());
        assert_eq!(terminal["type"], json!("error"), "{argv:?}");
        assert_eq!(run.exit_code(), ExitCode::USAGE, "{argv:?}");
    }

    // A handler-raised usage error, too.
    let run = stream_cli().run(["app", "deploy"]).await;
    let terminal = parse(&run.to_json());
    assert_eq!(terminal["type"], json!("error"));
    assert_eq!(terminal["error"]["code"], json!("MISSING_ARG"));
}

#[tokio::test]
async fn a_writer_kept_past_the_handler_is_closed() {
    // A handler that smuggles its writer out (here into a stash, in real code
    // into a spawned task) cannot write after the terminal line.
    let stash: Arc<Mutex<Option<StreamWriter>>> = Arc::default();
    let keep = Arc::clone(&stash);
    let cli = AgentCli::new("app", "x").command(
        Command::new("leak", "Keeps its writer")
            .usage("app leak")
            .stream_handler(move |_req, out| {
                let keep = Arc::clone(&keep);
                async move {
                    *keep.lock().unwrap() = Some(out);
                    Ok(CommandOutput::new(json!({})))
                }
            }),
    );
    let run = cli.run(["app", "leak"]).await;
    assert!(run.is_stream());

    let out = stash
        .lock()
        .unwrap()
        .take()
        .expect("the handler stashed it");
    let late = out.emit(&StreamEvent::Log {
        level: LogLevel::Warn,
        message: "too late".to_string(),
    });
    assert!(matches!(late, Err(StreamWriteError::Closed)), "{late:?}");
}

#[tokio::test]
async fn with_a_fixed_clock_every_line_carries_its_one_reading() {
    let run = stream_cli()
        .clock(|| 1_740_000_000)
        .run(["app", "deploy", "web"])
        .await;
    assert_eq!(run.stream_events().len(), 3);
    for line in run.stream_events() {
        assert_eq!(parse(line)["ts"], json!("2025-02-19T21:20:00Z"), "{line}");
    }
    assert_eq!(
        parse(&run.to_json())["timestamp"],
        json!("2025-02-19T21:20:00Z")
    );
}

#[tokio::test]
async fn the_clock_is_read_as_each_line_is_written() {
    // A counting clock: every reading is one second later than the last. The
    // events get increasing readings, and the terminal line, built after the
    // handler returned, gets the last one.
    let readings = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&readings);
    let run = stream_cli()
        .clock(move || counter.fetch_add(1, Ordering::SeqCst))
        .run(["app", "deploy", "web"])
        .await;

    let stamps: Vec<Value> = run
        .stream_events()
        .iter()
        .map(|line| parse(line)["ts"].clone())
        .collect();
    assert_eq!(
        stamps,
        [
            json!("1970-01-01T00:00:00Z"),
            json!("1970-01-01T00:00:01Z"),
            json!("1970-01-01T00:00:02Z"),
        ]
    );
    assert_eq!(
        parse(&run.to_json())["timestamp"],
        json!("1970-01-01T00:00:03Z")
    );
    assert_eq!(
        readings.load(Ordering::SeqCst),
        4,
        "one reading per line, nothing else"
    );
}

#[test]
fn a_stream_write_failure_is_a_stream_write_failed_error() {
    let error = CommandError::from(StreamWriteError::Poisoned);
    assert_eq!(error.code, ErrorCode::STREAM_WRITE_FAILED);
    assert!(error.message.contains("poisoned"), "{}", error.message);
    assert!(error.fix.contains("Re-run"), "{}", error.fix);
}

#[tokio::test]
async fn streaming_commands_are_marked_in_the_tree_help_and_skill() {
    let cli = stream_cli()
        .command(
            Command::new("status", "Status")
                .usage("app status")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
        )
        .skill();

    let root = parse(&cli.run(["app"]).await.to_json());
    let commands = root["result"]["commands"].as_array().expect("commands");
    let marked = |name: &str| {
        commands
            .iter()
            .find(|command| command["name"] == json!(name))
            .map(|command| command.get("stream").cloned())
            .expect("command listed")
    };
    assert_eq!(marked("deploy"), Some(json!(true)));
    assert_eq!(marked("status"), None, "only streaming commands are marked");

    let help = parse(&cli.run(["app", "help", "deploy"]).await.to_json());
    assert_eq!(help["result"]["stream"], json!(true));
    let help = parse(&cli.run(["app", "help", "status"]).await.to_json());
    assert!(help["result"].get("stream").is_none());

    let skill = cli.skill_markdown();
    let deploy = skill
        .lines()
        .find(|line| line.contains("`app deploy <service>`"))
        .expect("deploy is listed");
    assert!(deploy.contains("(stream:"), "{deploy}");
    assert!(
        skill.contains("A command marked stream prints NDJSON"),
        "{skill}"
    );
}

#[cfg(feature = "deserialize")]
#[tokio::test]
async fn every_line_parses_as_a_stream_line() {
    use agcli::StreamLine;

    let run = stream_cli().run(["app", "deploy", "web"]).await;
    for line in run.stream_events() {
        let parsed: StreamLine = serde_json::from_str(line).expect("event line parses");
        assert!(matches!(parsed, StreamLine::Event { .. }), "{line}");
    }
    let parsed: StreamLine = serde_json::from_str(&run.to_json()).expect("terminal parses");
    let StreamLine::Result(envelope) = parsed else {
        panic!("expected the result terminal, got {parsed:?}");
    };
    assert_eq!(envelope.result["service"], json!("web"));

    let run = stream_cli().run(["app", "fail"]).await;
    let parsed: StreamLine = serde_json::from_str(&run.to_json()).expect("terminal parses");
    assert!(matches!(parsed, StreamLine::Error(_)), "{parsed:?}");
}
