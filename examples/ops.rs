use agcli::{
    ActionParam, AgentCli, Command, CommandError, CommandOutput, NextAction,
    truncate_lines_with_file,
};
use serde_json::json;

#[tokio::main]
async fn main() {
    let cli = AgentCli::new("ops", "Agent-native operations CLI")
        .version(env!("CARGO_PKG_VERSION"))
        .root_field("health", json!({ "server": "ok", "worker": "ok" }))
        .command(
            Command::new("status", "Show system health")
                .usage("ops status")
                .handler(|_req, _ctx| {
                    Box::pin(async move {
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
                    })
                }),
        )
        .command(
            Command::new("logs", "View logs with context-safe truncation")
                .usage("ops logs <source> [--lines=<lines>] [--follow]")
                .handler(|req, _ctx| {
                    let source = req.arg(0).unwrap_or("worker").to_string();
                    // Floor agent-supplied --lines to 1 so `--lines=0` returns
                    // the tail line rather than an empty inline view.
                    let lines = req
                        .flag("lines")
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(20)
                        .max(1);

                    Box::pin(async move {
                        let fake_logs = (0..120)
                            .map(|idx| format!("[{source}] line-{idx}"))
                            .collect::<Vec<_>>();
                        let payload = truncate_lines_with_file(fake_logs, lines, "ops-logs")
                            .await
                            .map_err(|_| {
                                CommandError::new(
                                    "failed to write full log output",
                                    "LOG_WRITE_FAILED",
                                    "Check disk permissions and retry.",
                                )
                            })?;

                        Ok(
                            CommandOutput::new(json!(payload)).next_action(NextAction::new(
                                "ops logs <source> [--lines=<lines>] [--follow]",
                                "Adjust line count or follow logs",
                            )),
                        )
                    })
                }),
        )
        // A raw passthrough command: it owns its argv, its stdout, and its
        // exit code. Nothing here is parsed, projected, or wrapped in an
        // envelope — the shape a `grep` or a `cat` needs.
        .command(
            Command::new(
                "echo",
                "Print each argument on its own line (raw passthrough)",
            )
            .usage("ops echo [args...]")
            .raw_handler(|args, _ctx| {
                let args = args.to_vec();
                Box::pin(async move {
                    for arg in &args {
                        println!("{arg}");
                    }
                    // grep's convention: 1 means "nothing matched", not
                    // "the command failed".
                    i32::from(args.is_empty())
                })
            }),
        );

    // Prints the envelope on stdout — or nothing at all, when a raw command
    // already wrote its own — then exits with the typed code.
    cli.run_env().await.finish()
}
