# agcli - Agent-Native CLI Framework for Rust

`agcli` is a no-bloat Rust crate for building CLIs that AI agents can reliably operate. It implements 5 principles:

1. **JSON always** - every command returns structured JSON envelopes, never plain text
2. **HATEOAS** - every response includes `next_actions` telling the agent what to do next
3. **Self-documenting tree** - root command returns the full command tree as JSON
4. **Context protection** - truncation helpers cap large outputs with file pointers
5. **Errors suggest fixes** - error envelopes include `fix` and `retryable` fields

## Add as dependency

```toml
[dependencies]
agcli = "0.14.0"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Minimal calculator example

A complete, runnable CLI with `add` and `sub` commands:

> This is the canonical getting-started example, kept in sync with the
> CI-compiled [`examples/calc.rs`](examples/calc.rs). The crate is fully async
> (since v0.8): handlers return `Box::pin(async move { ... })` and `run_env`
> is awaited under a tokio runtime.

```rust
use agcli::{ActionParam, AgentCli, Command, CommandOutput, NextAction};
use serde_json::json;

#[tokio::main]
async fn main() {
    let cli = AgentCli::new("calc", "Agent-native calculator")
        .version("1.0.0")
        .command(
            Command::new("add", "Add two numbers")
                .usage("calc add <a> <b>")
                .handler(|req, _ctx| {
                    // `arg_parse` folds missing/invalid into a typed
                    // CommandError (MISSING_ARG / INVALID_ARG) with a fix.
                    // Borrow the request before the `async move` block.
                    let a = req.arg_parse::<f64>(0, "a");
                    let b = req.arg_parse::<f64>(1, "b");
                    Box::pin(async move {
                        let sum = a? + b?;
                        Ok(CommandOutput::new(json!({
                            "operation": "add",
                            "result": sum
                        }))
                        .next_action(
                            NextAction::new("calc add <a> <b>", "Add two more numbers")
                                .with_param("a", ActionParam::new()
                                    .value(json!(sum))
                                    .description("First number (pre-filled with previous result)"))
                                .with_param("b", ActionParam::new()
                                    .required(true)
                                    .description("Second number")),
                        )
                        .next_action(
                            NextAction::new("calc sub <a> <b>", "Subtract instead")
                                .with_param("a", ActionParam::new().value(json!(sum)))
                                .with_param("b", ActionParam::new().required(true)),
                        ))
                    })
                }),
        )
        .command(
            Command::new("sub", "Subtract two numbers")
                .usage("calc sub <a> <b>")
                .handler(|req, _ctx| {
                    let a = req.arg_parse::<f64>(0, "a");
                    let b = req.arg_parse::<f64>(1, "b");
                    Box::pin(async move {
                        let diff = a? - b?;
                        Ok(CommandOutput::new(json!({
                            "operation": "sub",
                            "result": diff
                        }))
                        .next_action(
                            NextAction::new("calc sub <a> <b>", "Subtract two more numbers")
                                .with_param("a", ActionParam::new().value(json!(diff)))
                                .with_param("b", ActionParam::new().required(true)),
                        ))
                    })
                }),
        );

    let run = cli.run_env().await;
    println!("{}", run.to_json());
    std::process::exit(run.exit_code());
}
```

## Build and run

```bash
# Run the shipped example directly:
cargo run --example calc                 # root command — self-documenting tree
cargo run --example calc -- add 3 5      # Add
cargo run --example calc -- sub 10 4     # Subtract
cargo run --example calc -- add foo bar  # Error case (typed-error envelope)
```

## Example JSON output

### Success (`calc add 3 5`)

```json
{
  "ok": true,
  "command": "calc add 3 5",
  "timestamp": "2025-02-19T21:20:00Z",
  "exit_code": 0,
  "result": {
    "operation": "add",
    "result": 8.0
  },
  "next_actions": [
    {
      "command": "calc add <a> <b>",
      "description": "Add two more numbers",
      "params": {
        "a": { "value": 8.0, "description": "First number (pre-filled with previous result)" },
        "b": { "required": true, "description": "Second number" }
      }
    },
    {
      "command": "calc sub <a> <b>",
      "description": "Subtract instead",
      "params": {
        "a": { "value": 8.0, "description": "First number (pre-filled with previous result)" },
        "b": { "required": true, "description": "Number to subtract" }
      }
    }
  ]
}
```

### Error (`calc add foo bar`)

```json
{
  "ok": false,
  "command": "calc add foo bar",
  "timestamp": "2025-02-19T21:20:00Z",
  "exit_code": 2,
  "error": {
    "message": "argument <a> is not valid: \"foo\"",
    "code": "INVALID_ARG",
    "retryable": false
  },
  "fix": "Pass a valid value for <a>.",
  "next_actions": [
    {
      "command": "calc add <a> <b>",
      "description": "Run this command template",
      "params": {
        "a": { "required": true },
        "b": { "required": true }
      }
    },
    {
      "command": "calc",
      "description": "Inspect the full command tree"
    }
  ]
}
```

## Key patterns

- **Envelope structure**: Every response has `ok`, `command`, `timestamp` (RFC 3339 UTC string, e.g. `"2026-06-10T14:42:17Z"` — agents always know the current time), `exit_code`, `result`/`error`, `next_actions`
- **Typed exit codes**: `exit_code` is both the process status and a JSON field. Framework usage errors are `2`; handler errors default to `1` and opt into `ExitCode::{NOT_FOUND, AUTH, API, RATE_LIMITED, …}` via `CommandError::exit_code(...)`
- **Template vs literal next_actions**: When `params` is present, `command` is a template (agent fills placeholders). When absent, it's literal (run as-is).
- **Pre-filled values**: Use `ActionParam::new().value(json!(result))` to pre-fill context from the current operation
- **Error with fix**: `CommandError::new(message, code, fix)` - always tell the agent how to recover
- **Typed arg helpers**: `req.require_arg(i, "name")`, `req.arg_parse::<T>(i, "name")`, and `req.flag_parse::<T>("key")` fold missing/parse failures into a `CommandError` with conventional codes (`MISSING_ARG`/`INVALID_ARG`/`INVALID_FLAG`) and a generated `fix` — no hand-rolled boilerplate per argument
- **Retryable errors**: Chain `.retryable(true)` on `CommandError` for transient failures
- **Reserved flags are discoverable**: the root command tree includes an `agent_flags` section (when reserved flags are enabled) listing `--select`/`--compact`/`--quiet`/etc.; `agcli::reserved_flag_names()` returns the same set programmatically
- **`--select` never silently wipes output**: a bare, empty, or no-match `--select` returns the full result plus a `select_warning` (listing the available fields) instead of an empty `{}` with a misleading `ok: true`
- **Context protection is opt-in**: the framework does not bound result size by default — call `truncate_lines_with_file()` (or `CommandOutput::list_truncated`) deliberately for large output. The truncated tail is returned inline with a `dropped` count and a `full_output` file pointer; that file persists until the caller calls `TruncatedEntries::cleanup()`
- **List results advertise their row schema**: `CommandOutput::list`/`list_truncated` add a `fields` key holding the `--select` paths that cover a row — one sorted `items.<key>` dot path per distinct top-level key, e.g. `["items.id", "items.name"]` — and the framework appends a pre-filled `--select` `next_action` re-running that exact invocation, so an agent can make the cheaper projected call without first decoding a row. Dot paths because `--select` projects top-level keys: `--select=items.id,items.name` works, bare `id,name` hits the `select_warning`. The advertisement is suppressed when the caller already passed `--select`, under `--quiet`, and when reserved flags or the command's reserved projection are off; `fields` is emitted regardless
- **Streaming**: Use `NdjsonEmitter` for temporal operations; terminal `result`/`error` events carry `timestamp` and `schema_version`. The emitter poisons itself on a write/flush error so a retry can't concatenate onto a corrupt NDJSON line

## Performance

agcli targets **macOS and Linux only**. The crate ships with optimized release/bench profiles. Downstream binaries get maximum runtime performance with these settings:

### Recommended `Cargo.toml` for downstream binaries

```toml
[dependencies]
agcli = "0.14.0"

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
```

### Allocator

The default system allocator is the right choice for short-lived CLI processes. agcli does not bundle one. If you build a long-running, allocation-heavy CLI and *measure* a win, add `tikv-jemallocator` (or another allocator) directly in your binary:

```rust
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
```

### Build-machine-specific codegen

For maximum throughput on a known deployment target:

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

Do **not** bake this into the repo — it breaks cross-compilation and CI portability.

### PGO (Profile-Guided Optimization)

For the last few percent of throughput, use `cargo-pgo`:

```bash
cargo install cargo-pgo
cargo pgo build
# Run representative workload against the instrumented binary
./target/release/myapp <typical args>
cargo pgo optimize
```

## Release process (IMPORTANT — tag-driven, do NOT publish manually)

Releases are **automated by CI on tag push**. `.github/workflows/release.yml`
triggers on a `v[0-9]+.*` tag and runs: verify tag → test → **`cargo publish`**
(authenticated via the `crates-io` GitHub environment) → **GitHub Release**
with notes generated by git-cliff. The `github-release` job depends on the
`publish` job.

**Do NOT run `cargo publish` by hand.** Doing so pre-empts CI: the tag-push
`cargo publish` then fails with "crate version already exists", which also
blocks the dependent GitHub Release job. Let the tag drive everything.

Correct flow for a release `vX.Y.Z`:

1. Bump `version` in `Cargo.toml`; run `cargo check` to update `Cargo.lock`.
2. Add a `## [X.Y.Z]` section to `CHANGELOG.md` (breaking changes first).
3. Update the install snippets in `README.md` and this file to `X.Y.Z`.
4. Commit: `chore: release vX.Y.Z` (feature work goes in its own commit first).
5. `git push` to `master`.
6. `git tag vX.Y.Z && git push origin vX.Y.Z` — CI publishes + cuts the release.

Versioning: pre-1.0, breaking changes (schema/exit-code/feature removals) are a
**minor** bump (0.8 → 0.9), not patch.

> Historical note: **0.9.0 was published manually via `cargo publish`** (out of
> band). Because of that, a `v0.9.0` tag must **not** be pushed — its CI publish
> step would fail. From 0.9.1 onward, use the tag-driven flow above.
