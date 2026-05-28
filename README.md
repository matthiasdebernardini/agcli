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
agcli = "0.8.1"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The crate is 100% async (since v0.8). Handlers, the NDJSON emitter, and
truncation I/O all return `Future`s. Wire up a tokio runtime in your binary —
the snippet below uses `#[tokio::main]`.

## Quick start

```rust
use agcli::{AgentCli, Command, CommandOutput, NextAction};
use serde_json::json;

#[tokio::main]
async fn main() {
    let cli = AgentCli::new("ops", "Agent-native operations CLI")
        .version("0.8.0")
        .command(
            Command::new("status", "Show system health")
                .usage("ops status")
                .handler(|_req, _ctx| {
                    Box::pin(async move {
                        Ok(CommandOutput::new(json!({ "healthy": true })).next_action(
                            NextAction::new("ops status", "Re-check status"),
                        ))
                    })
                }),
        );

    let run = cli.run_env().await;
    println!("{}", run.to_json());
    std::process::exit(run.exit_code());
}
```

## Flag parsing

`agcli` accepts both `--key=value` and `--key value` (space-separated) for
value flags. This matches the HATEOAS `[--flag <value>]` template form used
throughout the crate's docs.

To disambiguate boolean flags from value flags without a schema layer, the
parser reads each command's `.usage(...)` string at runtime and treats any
bracketed flag without a `<placeholder>` (e.g. `[--no-git]`, `[--follow]`)
as a pure boolean. Declared boolean flags will never consume the next token,
so `mycli submit --no-git ./plan.html` works as expected.

For value flags or undeclared flags, a bare `--key` followed by a non-flag
token consumes that token as the value (`--key value` ≡ `--key=value`). Use
`--key=true` or `--` to force a positional after an undeclared boolean.

## Performance

agcli targets **macOS and Linux only**. The crate ships with optimized release/bench profiles and an optional jemalloc allocator. To maximize runtime performance in a downstream binary:

### Recommended `Cargo.toml`

```toml
[dependencies]
agcli = { version = "0.8.1", features = ["jemalloc"] }

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
