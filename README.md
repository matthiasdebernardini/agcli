# agcli

`agcli` is a no-bloat Rust crate for building agent-native CLIs.

It is built around the design in [design.md](design.md):
- JSON-only envelopes
- HATEOAS `next_actions`
- self-documenting root command tree
- context-safe output truncation
- typed NDJSON streaming with terminal `result`/`error`

## Install

```toml
[dependencies]
agcli = "0.1.0"
serde_json = "1"
```

## Quick start

```rust
use agcli::{AgentCli, Command, CommandOutput, NextAction};
use serde_json::json;

fn main() {
    let cli = AgentCli::new("ops", "Agent-native operations CLI")
        .version("0.1.0")
        .command(
            Command::new("status", "Show system health")
                .usage("ops status")
                .handler(|_req, _ctx| {
                    Ok(CommandOutput::new(json!({ "healthy": true })).next_action(
                        NextAction::new("ops status", "Re-check status"),
                    ))
                }),
        );

    let run = cli.run_env();
    println!("{}", run.to_json());
    std::process::exit(run.exit_code());
}
```

## Wokhei-style example

See [examples/ops.rs](examples/ops.rs) for a full example with:
- command tree responses
- contextual next actions
- log truncation file pointers
