# agcli

`agcli` is a no-bloat Rust crate for building agent-native CLIs.

It is built around the design in [design.md](design.md):
- JSON-only envelopes
- HATEOAS `next_actions`
- self-documenting root command tree
- typed exit codes for agent self-correction
- agent-native output flags (`--select`, `--compact`, `--quiet`) on every command
- context-safe output truncation and bounded list output
- built-in `doctor` health checks and a static command-tree self-audit
- typed NDJSON streaming with terminal `result`/`error`

## Why terminal envelopes and truncation pointers matter

- Terminal `result` / `error` envelopes give agents a deterministic finish state, so they can branch on structured outcomes instead of fragile text parsing.
- Structured `error` envelopes support reliable retries, escalation, and fallback actions, while `result` envelopes make successful completion explicit and machine-verifiable.
- Truncation with file pointers lets CLIs cap large outputs safely while preserving continuity: agents can follow the pointer to full logs or artifacts without overflowing context windows.
- This improves reliability and debuggability for long-running automation while reducing token pressure in agent loops.

## Install

```toml
[dependencies]
agcli = "0.12.0"
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

## Agent-native flags

Every command supports a reserved vocabulary of agent-native flags without
declaring them. The framework applies three of them to the result centrally,
so any command gets token-economy output for free:

| Flag | Effect |
|------|--------|
| `--select=a,b,c` | Project the result to only these fields (top-level keys or `a.b` dot paths; maps over arrays). |
| `--compact` | Drop `null` and empty fields. A command may instead declare `CommandOutput::compact_fields([...])` to keep an explicit high-gravity allowlist. |
| `--quiet` | Omit `next_actions` from the envelope. |

The rest are parsed as booleans anywhere on the line and exposed via typed
accessors on `CommandRequest`, for the handler to act on:

`--dry-run` → `req.dry_run()`, `--yes`/`--no-input` → `req.assume_yes()`,
`--no-cache` → `req.no_cache()`, `--no-color` → `req.no_color()`,
`--stdin` → `req.wants_stdin()` (pair with `agcli::read_stdin().await`).

These names are reserved while enabled (the default). If a command needs one
with conflicting semantics, opt out per-CLI with `AgentCli::reserved_flags(false)`.

The reserved vocabulary is **discoverable**: the root command tree includes an
`agent_flags` section describing every flag (so an introspecting agent finds
the full surface it can drive), and `agcli::reserved_flag_names()` returns the
same set programmatically. A bare, empty, or no-match `--select` never silently
wipes the result to `{}` — it returns the full result plus a `select_warning`
listing the available fields.

```rust
// `app get 1 --select id,name --compact` → result projected and compacted,
// even though `get` never declared those flags.
```

## Typed exit codes

Every envelope carries a typed exit code as a first-class field — both as the
process exit status (`Execution::exit_code()`) **and** in the JSON
(`"exit_code": N`) — so an agent can branch on the failure class whether it
reads `$?` from a shell or parses stdout. Framework usage errors (unknown
command, bad flag, missing handler) return `2`; handler errors default to `1`
and opt into a typed code via `CommandError::exit_code(...)`.

| `ExitCode` | Value | Meaning |
|------------|-------|---------|
| `SUCCESS` | 0 | Command succeeded |
| `ERROR` | 1 | Generic failure (default for handler errors) |
| `USAGE` | 2 | Bad invocation (framework-raised) |
| `NOT_FOUND` | 3 | Requested resource missing |
| `AUTH` | 4 | Auth/authorization failure |
| `API` | 5 | Upstream/API call failed |
| `RATE_LIMITED` | 7 | Back off and retry later |

```rust
Err(CommandError::new("no such issue", "NOT_FOUND", "Check the id")
    .exit_code(agcli::ExitCode::NOT_FOUND))
```

```json
{ "ok": false, "command": "app get 9", "timestamp": "2025-02-19T21:20:00Z",
  "exit_code": 3, "error": { "message": "no such issue", "code": "NOT_FOUND",
  "retryable": false }, "fix": "Check the id", "next_actions": [ ... ] }
```

## Bounded lists

`CommandOutput::list(items)` and `CommandOutput::list_truncated(items, total)`
emit a bounded `{ items, count, total, truncated }` result. When truncated,
a `guidance` field tells the agent how to narrow the query.

## Built-in `doctor` and self-audit

`AgentCli::doctor(checks)` registers a `doctor` command that runs your
[`Check`]s and reports `{ healthy, checks: [...] }`. A failing check still
produces an `ok: true` envelope (the report ran) but carries that check's
exit code (e.g. `ExitCode::AUTH`), so the shell sees a non-zero status.

`AgentCli::audit()` statically validates the command tree and returns an
`AuditReport`: it flags dangling `next_action` templates (HATEOAS integrity),
dead-end commands, and missing usage/descriptions. Use it in a test:

```rust
assert!(cli.audit().is_clean());
```

## Performance

agcli targets **macOS and Linux only**. The crate ships with optimized release/bench profiles. To maximize runtime performance in a downstream binary:

### Recommended `Cargo.toml`

```toml
[dependencies]
agcli = "0.12.0"

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
```

The default system allocator is the right choice for short-lived CLI
processes. If you build a long-running, allocation-heavy CLI and measure a
win, add `tikv-jemallocator` (or another allocator) directly in your binary
via `#[global_allocator]` — agcli does not bundle one.

## Full example

See [examples/ops.rs](examples/ops.rs) — a runnable `ops` CLI demonstrating:
- the self-documenting command tree
- contextual `next_actions`
- log truncation with file pointers
