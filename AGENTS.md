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
agcli = "0.10.1"
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
  "exit_code": 1,
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
- **Template vs literal next_actions**: When `params` is present, `command` is a template (agent fills placeholders). When absent, it's literal (run as-is).
- **Pre-filled values**: Use `ActionParam::new().value(json!(result))` to pre-fill context from the current operation
- **Typed arg helpers**: `req.require_arg(i, "name")`, `req.arg_parse::<T>(i, "name")`, and `req.flag_parse::<T>("key")` fold missing/parse failures into a `CommandError` with conventional codes (`MISSING_ARG`/`INVALID_ARG`/`INVALID_FLAG`) and a generated `fix` — no per-argument boilerplate
- **Error with fix**: `CommandError::new(message, code, fix)` - always tell the agent how to recover
- **Retryable errors**: Chain `.retryable(true)` on `CommandError` for transient failures
- **Truncation**: Use `truncate_lines_with_file()` to cap large outputs and write full content to a temp file
- **Streaming**: Use `NdjsonEmitter` for temporal operations; terminal `result`/`error` events carry `timestamp` and `schema_version`
