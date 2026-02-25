use agcli::{
    ActionParam, AgentCli, Command, CommandError, CommandOutput, NextAction,
    truncate_lines_with_file,
};
use serde_json::json;

fn main() {
    let cli = AgentCli::new("ops", "Agent-native operations CLI")
        .version(env!("CARGO_PKG_VERSION"))
        .root_field("health", json!({ "server": "ok", "worker": "ok" }))
        .command(
            Command::new("status", "Show system health")
                .usage("ops status")
                .handler(|_req, _ctx| {
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
                .handler(|req, _ctx| {
                    let source = req.arg(0).unwrap_or("worker");
                    let lines = req
                        .flag("lines")
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(20);

                    let fake_logs = (0..120)
                        .map(|idx| format!("[{source}] line-{idx}"))
                        .collect::<Vec<_>>();
                    let payload =
                        truncate_lines_with_file(fake_logs, lines, "ops-logs").map_err(|_| {
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
                }),
        );

    let run = cli.run_env();
    println!("{}", run.to_json());
    std::process::exit(run.exit_code());
}
