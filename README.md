# agcli

`agcli` is a no-bloat Rust crate for building agent-native CLIs.

It is built around the design in [design.md](design.md):
- JSON-only envelopes
- HATEOAS `next_actions`
- self-documenting root command tree
- context-safe output truncation
- typed NDJSON streaming with terminal `result`/`error`

## Why terminal envelopes and truncation pointers matter

- Terminal `result` / `error` envelopes give agents a deterministic finish state, so they can branch on structured outcomes instead of fragile text parsing.
- Structured `error` envelopes support reliable retries, escalation, and fallback actions, while `result` envelopes make successful completion explicit and machine-verifiable.
- Truncation with file pointers lets CLIs cap large outputs safely while preserving continuity: agents can follow the pointer to full logs or artifacts without overflowing context windows.
- This improves reliability and debuggability for long-running automation while reducing token pressure in agent loops.

## Install

```toml
[dependencies]
agcli = "0.6.0"
serde_json = "1"
```

## Quick start

```rust
use agcli::{AgentCli, Command, CommandOutput, NextAction};
use serde_json::json;

fn main() {
    let cli = AgentCli::new("ops", "Agent-native operations CLI")
        .version("0.4.0")
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

## Performance

agcli targets **macOS and Linux only**. The crate ships with optimized release/bench profiles and an optional jemalloc allocator. To maximize runtime performance in a downstream binary:

### Recommended `Cargo.toml`

```toml
[dependencies]
agcli = { version = "0.6.0", features = ["jemalloc"] }

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
```

### jemalloc global allocator

In your binary's `main.rs`:

```rust
#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: agcli::Jemalloc = agcli::Jemalloc;
```

### Build-machine-specific codegen

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

Do **not** commit this into the repo — it breaks cross-compilation portability.

### PGO (Profile-Guided Optimization)

```bash
cargo install cargo-pgo
cargo pgo build
./target/release/myapp <typical args>   # run representative workload
cargo pgo optimize
```

## Wokhei-style example

See [examples/ops.rs](examples/ops.rs) for a full example with:
- command tree responses
- contextual next actions
- log truncation file pointers
