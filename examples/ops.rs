use std::num::NonZeroUsize;

use agcli::{
    ActionParam, AgentCli, Command, CommandError, CommandOutput, ErrorCode, ExitCode, NextAction,
    StepStatus, StreamEvent, truncate_lines_with_file,
};
use serde_json::json;

/// The steps `ops deploy` walks through, in order, with the share of the
/// deploy done once each finishes.
const DEPLOY_STEPS: [(&str, u8); 3] = [("build", 33), ("upload", 66), ("activate", 100)];

/// A streaming command: NDJSON step and progress events while it works, then
/// the framework's one terminal `"type":"result"` line.
fn deploy_command() -> Command {
    Command::new("deploy", "Deploy a service, streaming each step")
        .usage("ops deploy <service>")
        .stream_handler(|req, mut out| async move {
            let service = req.require_arg(0, "service")?.to_string();
            out.emit(&StreamEvent::Start {
                command: format!("ops deploy {service}"),
            })?;
            for (step, percent) in DEPLOY_STEPS {
                out.emit(&StreamEvent::Step {
                    name: step.to_string(),
                    status: StepStatus::Started,
                    duration_ms: None,
                    error: None,
                })?;
                out.emit(&StreamEvent::Step {
                    name: step.to_string(),
                    status: StepStatus::Completed,
                    duration_ms: Some(0),
                    error: None,
                })?;
                out.emit(&StreamEvent::Progress {
                    name: "deploy".to_string(),
                    percent: Some(percent),
                    message: Some(format!("{step} done")),
                })?;
            }
            let steps: Vec<&str> = DEPLOY_STEPS.iter().map(|(step, _)| *step).collect();
            Ok(CommandOutput::new(json!({
                "service": service,
                "deployed": true,
                "steps": steps,
            }))
            .next_action(NextAction::new(
                "ops status",
                "Check health after the deploy",
            )))
        })
}

/// The whole `ops` CLI.
fn cli() -> AgentCli {
    AgentCli::new("ops", "Agent-native operations CLI")
        .version(env!("CARGO_PKG_VERSION"))
        .root_field("health", json!({ "server": "ok", "worker": "ok" }))
        .command(
            Command::new("status", "Show system health")
                .usage("ops status")
                .handler(|_req| async move {
                    Ok(CommandOutput::new(json!({
                        "healthy": true,
                        "queue_depth": 0
                    }))
                    .next_action(NextAction::new("ops status", "Re-check status"))
                    .next_action(
                        NextAction::new("ops logs <source> [--lines=<lines>]", "Inspect logs")
                            .with_param(
                                "source",
                                ActionParam::new()
                                    .enum_values(["worker", "errors", "server"])
                                    .default("worker"),
                            )
                            .with_param(
                                "lines",
                                ActionParam::new()
                                    .description("Number of lines to show")
                                    .default(20),
                            ),
                    ))
                }),
        )
        .command(
            Command::new("logs", "View logs with context-safe truncation")
                .usage("ops logs <source> [--lines=<lines>] [--follow]")
                .handler(|req| async move {
                    // A missing <source> or a `--lines` that is not a positive
                    // count is a typed usage error, never a quiet default.
                    let source = req.require_arg(0, "source")?.to_string();
                    let lines = req
                        .flag_parse::<NonZeroUsize>("lines")?
                        .map_or(20, NonZeroUsize::get);

                    let fake_logs: Vec<_> = (0..120)
                        .map(|idx| format!("[{source}] line-{idx}"))
                        .collect();
                    let payload = truncate_lines_with_file(fake_logs, lines, "ops-logs").map_err(
                        |error| {
                            CommandError::new(
                                ErrorCode::new("LOG_WRITE_FAILED"),
                                format!("failed to write full log output: {error}"),
                                "Check disk permissions and retry.",
                            )
                        },
                    )?;

                    Ok(
                        CommandOutput::new(json!(payload)).next_action(NextAction::new(
                            "ops logs <source> [--lines=<lines>] [--follow]",
                            "Adjust line count or follow logs",
                        )),
                    )
                }),
        )
        .command(deploy_command())
        // A raw passthrough command: it owns its argv, its stdout, and its
        // exit code. Nothing here is parsed, projected, or wrapped in an
        // envelope — the shape a `grep` or a `cat` needs.
        .command(
            Command::new(
                "echo",
                "Print each argument on its own line (raw passthrough)",
            )
            .usage("ops echo [args...]")
            .raw_handler(|args| async move {
                for arg in &args {
                    println!("{arg}");
                }
                // grep's convention: 1 means "nothing matched", not
                // "the command failed".
                ExitCode::new(u8::from(args.is_empty()))
            }),
        )
}

#[tokio::main]
async fn main() {
    // Prints the envelope on stdout — or nothing at all, when a raw command
    // already wrote its own — then exits with the typed code.
    cli().run_env().await.finish()
}
