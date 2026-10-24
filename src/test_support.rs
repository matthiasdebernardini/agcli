//! CLIs shared by the unit tests of several modules.

use serde_json::json;

use crate::{AgentCli, Command, CommandOutput, NextAction};

#[path = "../tests/support/literal_actions.rs"]
pub(crate) mod literal_actions;

pub(crate) fn sample_cli() -> AgentCli {
    AgentCli::new("wokhei", "Agent-first Nostr list CLI")
        .version("0.1.0")
        .command(
            Command::new("status", "Show relay and key status")
                .usage("wokhei status")
                .handler(|_req| async move {
                    Ok(CommandOutput::new(json!({
                        "healthy": true,
                        "keys_configured": true
                    }))
                    .next_action(NextAction::new("wokhei status", "Re-check current status")))
                }),
        )
        .command(
            Command::new("gateway", "Gateway operations").subcommand(
                Command::new("stream", "Stream gateway events")
                    .usage("wokhei gateway stream [--follow]")
                    .handler(|_req| async move {
                        Ok(CommandOutput::new(json!({
                            "stream": "ready"
                        })))
                    }),
            ),
        )
}

pub(crate) fn rich_cli() -> AgentCli {
    AgentCli::new("app", "Rich output CLI").command(
        Command::new("get", "Get a record")
            .usage("app get <id>")
            .handler(|_req| async move {
                Ok(CommandOutput::new(json!({
                    "id": 1,
                    "name": "widget",
                    "note": null,
                    "tags": [],
                    "body": "a very long body field"
                })))
            }),
    )
}
