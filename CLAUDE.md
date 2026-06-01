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
agcli = "0.9.0"
serde_json = "1"
```

## Minimal calculator example

A complete, runnable CLI with `add` and `sub` commands:

```rust
use agcli::{AgentCli, Command, CommandError, CommandOutput, NextAction, ActionParam};
use serde_json::json;

fn main() {
    let cli = AgentCli::new("calc", "Agent-native calculator")
        .version("1.0.0")
        .command(
            Command::new("add", "Add two numbers")
                .usage("calc add <a> <b>")
                .handler(|req, _ctx| {
                    let a: f64 = req.arg(0)
                        .ok_or_else(|| CommandError::new(
                            "missing argument <a>",
                            "MISSING_ARG",
                            "Provide two numbers: calc add <a> <b>",
                        ))?
                        .parse()
                        .map_err(|_| CommandError::new(
                            "argument <a> is not a number",
                            "INVALID_NUMBER",
                            "Pass a valid number for <a>",
                        ))?;
                    let b: f64 = req.arg(1)
                        .ok_or_else(|| CommandError::new(
                            "missing argument <b>",
                            "MISSING_ARG",
                            "Provide two numbers: calc add <a> <b>",
                        ))?
                        .parse()
                        .map_err(|_| CommandError::new(
                            "argument <b> is not a number",
                            "INVALID_NUMBER",
                            "Pass a valid number for <b>",
                        ))?;

                    let sum = a + b;
                    Ok(CommandOutput::new(json!({
                        "operation": "add",
                        "a": a,
                        "b": b,
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
                            .with_param("a", ActionParam::new()
                                .value(json!(sum))
                                .description("First number (pre-filled with previous result)"))
                            .with_param("b", ActionParam::new()
                                .required(true)
                                .description("Number to subtract")),
                    ))
                }),
        )
        .command(
            Command::new("sub", "Subtract two numbers")
                .usage("calc sub <a> <b>")
                .handler(|req, _ctx| {
                    let a: f64 = req.arg(0)
                        .ok_or_else(|| CommandError::new(
                            "missing argument <a>",
                            "MISSING_ARG",
                            "Provide two numbers: calc sub <a> <b>",
                        ))?
                        .parse()
                        .map_err(|_| CommandError::new(
                            "argument <a> is not a number",
                            "INVALID_NUMBER",
                            "Pass a valid number for <a>",
                        ))?;
                    let b: f64 = req.arg(1)
                        .ok_or_else(|| CommandError::new(
                            "missing argument <b>",
                            "MISSING_ARG",
                            "Provide two numbers: calc sub <a> <b>",
                        ))?
                        .parse()
                        .map_err(|_| CommandError::new(
                            "argument <b> is not a number",
                            "INVALID_NUMBER",
                            "Pass a valid number for <b>",
                        ))?;

                    let diff = a - b;
                    Ok(CommandOutput::new(json!({
                        "operation": "sub",
                        "a": a,
                        "b": b,
                        "result": diff
                    }))
                    .next_action(
                        NextAction::new("calc sub <a> <b>", "Subtract two more numbers")
                            .with_param("a", ActionParam::new()
                                .value(json!(diff))
                                .description("First number (pre-filled with previous result)"))
                            .with_param("b", ActionParam::new()
                                .required(true)
                                .description("Number to subtract")),
                    ))
                }),
        );

    let run = cli.run_env();
    println!("{}", run.to_json());
    std::process::exit(run.exit_code());
}
```

## Build and run

```bash
cargo build --release
# Root command - self-documenting tree
./target/release/calc
# Add
./target/release/calc add 3 5
# Subtract
./target/release/calc sub 10 4
# Error case
./target/release/calc add foo bar
```

## Example JSON output

### Success (`calc add 3 5`)

```json
{
  "ok": true,
  "command": "calc add 3 5",
  "timestamp": 1740000000,
  "exit_code": 0,
  "result": {
    "operation": "add",
    "a": 3.0,
    "b": 5.0,
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
  "timestamp": 1740000000,
  "exit_code": 1,
  "error": {
    "message": "argument <a> is not a number",
    "code": "INVALID_NUMBER",
    "retryable": false
  },
  "fix": "Pass a valid number for <a>",
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

- **Envelope structure**: Every response has `ok`, `command`, `timestamp`, `exit_code`, `result`/`error`, `next_actions`
- **Typed exit codes**: `exit_code` is both the process status and a JSON field. Framework usage errors are `2`; handler errors default to `1` and opt into `ExitCode::{NOT_FOUND, AUTH, API, RATE_LIMITED, …}` via `CommandError::exit_code(...)`
- **Template vs literal next_actions**: When `params` is present, `command` is a template (agent fills placeholders). When absent, it's literal (run as-is).
- **Pre-filled values**: Use `ActionParam::new().value(json!(result))` to pre-fill context from the current operation
- **Error with fix**: `CommandError::new(message, code, fix)` - always tell the agent how to recover
- **Retryable errors**: Chain `.retryable(true)` on `CommandError` for transient failures
- **Truncation**: Use `truncate_lines_with_file()` to cap large outputs and write full content to a temp file
- **Streaming**: Use `NdjsonEmitter` for temporal operations; terminal `result`/`error` events carry `timestamp` and `schema_version`

## Performance

agcli targets **macOS and Linux only**. The crate ships with optimized release/bench profiles. Downstream binaries get maximum runtime performance with these settings:

### Recommended `Cargo.toml` for downstream binaries

```toml
[dependencies]
agcli = "0.9.0"

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
