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
- raw passthrough commands for the few that owe a foreign output contract

## Why terminal envelopes and truncation pointers matter

- Terminal `result` / `error` envelopes give agents a deterministic finish state, so they can branch on structured outcomes instead of fragile text parsing.
- Structured `error` envelopes support reliable retries, escalation, and fallback actions, while `result` envelopes make successful completion explicit and machine-verifiable.
- Truncation with file pointers lets CLIs cap large outputs safely while preserving continuity: agents can follow the pointer to full logs or artifacts without overflowing context windows.
- This improves reliability and debuggability for long-running automation while reducing token pressure in agent loops.

## Install

```toml
[dependencies]
agcli = "0.16.0"
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

    // Prints the envelope on stdout, then exits with the typed code.
    cli.run_env().await.finish()
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

`--json` is reserved too, and does nothing: output is always JSON. It exists so
a CLI can *drop* its own `--json` flag without breaking the calls agents already
have memorized. Because it is reserved as a **boolean**, it never consumes the
next token — `app brain --json myrepo` keeps `myrepo` as a positional. The same
holds for any undeclared flag the command has opted to allow: only the reserved
booleans are guaranteed not to swallow what follows them (declare a bool flag in
`.usage(...)` to get the same guarantee for your own).

These names are reserved while enabled (the default). If a command needs one
with conflicting semantics, opt out per-CLI with `AgentCli::reserved_flags(false)`
— which also gives up the `--json` back-compat above.

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

### Structured failure payloads

`message` and `fix` are prose. When the failure also carries facts the agent
must act on — which rows were rejected and why, what the server replied —
attach them with `CommandError::data(...)` instead of writing them into the
message for the caller to parse back out:

```rust
Err(CommandError::new("2 of 3 comments rejected", "PARTIAL_REJECT",
        "Fix the line numbers and re-post the rejected comments")
    .data(json!({ "rejected": [ { "line": 91, "why": "past end of file" } ] })))
```

The payload becomes a `data` key on the error envelope, next to `error` and
`fix` — and on the NDJSON terminal `error` event, in the same place, so the
failure reads the same whichever channel the caller chose. Set nothing and the
key is absent.

## Bounded lists

`CommandOutput::list(items)` and `CommandOutput::list_truncated(items, total)`
emit a bounded `{ items, count, total, truncated }` result. When truncated,
a `guidance` field tells the agent how to narrow the query.

Both also publish the row schema and teach the cheaper call. A `fields` key
lists the `--select` paths that cover a row — one `items.<key>` dot path per
distinct top-level key across the items, sorted (omitted when the rows are not
objects, or the list is empty) — and the framework appends a `next_action`
that re-runs *this exact invocation* under `--select`:

```json
{ "result": { "items": [ { "id": 1, "name": "a" } ], "count": 1, "total": 1,
              "truncated": false, "fields": ["items.id", "items.name"] },
  "next_actions": [
    { "command": "app ls --select=<fields>",
      "description": "Re-run projected to only the fields you need — smaller result, same data",
      "params": { "fields": { "required": true,
                              "description": "Comma-separated subset of: items.id, items.name (dot paths project each row)" } } } ] }
```

They are dot paths because `--select` projects *top-level* result keys: a bare
`id` would miss the rows nested under `items` and come back as a
`select_warning`. Paste them back unedited —

```bash
app ls --select=items.id,items.name
# => { "items": [ { "id": 1, "name": "a" } ] }
```

— and the metadata keys (`count`, `total`, `truncated`, `fields`) drop out
with everything else you did not ask for. Projection is the biggest token win
available on a list result, and the agent no longer has to decode a row to
learn the field names. The advertisement is skipped when `--select` is already
in use, under `--quiet` (which strips `next_actions` entirely), and when
reserved flags or the command's reserved projection are disabled. `fields`
itself is schema disclosure and is emitted either way.

## Raw passthrough commands

Most commands should answer with an envelope. A few cannot: a `grep` that owes
callers `path:line:content` on stdout and exit `1` for "no matches", a `cat`, a
shim around another program. Wrapping those in JSON does not make them
agent-native, it makes them wrong.

`Command::raw_handler(...)` is the opt-out. The handler receives the verbatim
argv tail as `&[String]`, writes stdout itself, and returns the process exit
code:

```rust
Command::new("grep", "Search the index (ripgrep-compatible output)")
    .usage("app grep [rg-flags...] <pattern> [path...]")
    .raw_handler(|args, _ctx| {
        let args = args.to_vec();
        Box::pin(async move {
            let hits = search(&args).await;
            for hit in &hits {
                println!("{}:{}:{}", hit.path, hit.line, hit.text);
            }
            i32::from(hits.is_empty()) // rg's convention: 1 = no matches
        })
    })
```

What the framework does *not* do for a raw command:

| Skipped | Why it matters |
|---------|----------------|
| Flag parsing | `-C 3`, `-t rust`, `-g '*.rs'`, `--`, and patterns that start with `-` arrive untouched and in order. |
| Unknown-flag rejection | An unrecognized flag is the handler's business, not a `UNKNOWN_FLAG` error. |
| Positional-arity checks | No `EXTRA_ARG` on a long argument list. |
| The `--dry-run` gate | The token is passed through like any other. |
| `--select` / `--compact` / `--quiet` projection | There is no envelope to project. |
| Envelope serialization and `next_actions` | Stdout carries exactly what the handler printed. |

What it still does: the command appears in the root command tree (marked
`"raw": true` so an introspecting agent knows it answers in raw text), in
`help`, and in `audit()`. Panics are still caught — a panicking raw handler
exits `1` and its `HANDLER_PANIC` envelope goes to **stderr**, never onto the
stdout it was in the middle of writing. The exit code the handler returns is
truncated to its low 8 bits, so forwarding another program's
`status.code().unwrap_or(-1)` yields `255` — the same value the shell sees.

The guarantee is unconditional. It does not depend on where the command's name
sits on the line, and it survives a line the parser cannot read:

```bash
app grep -h pat            # -h belongs to grep, not to the framework
app --json grep -h pat     # a global flag first changes nothing
app --json grep --=x       # even an unparseable token reaches the handler
app help grep              # …and this is how you ask the framework instead
```

Finish `main` with `Execution::finish()` — or check `Execution::is_raw()`
before printing anything yourself:

```rust
let run = cli.run_env().await;
if !run.is_raw() {
    println!("{}", run.to_json()); // a raw command already wrote its stdout
}
std::process::exit(run.exit_code());
```

`finish()` ends in `std::process::exit`, which runs no destructors: a raw
handler that buffers its own writer must flush before it returns. `println!`
already flushes each line.

A raw command is a leaf, and it replaces the normal handler. `audit()` reports
both mistakes: `RAW_COMMAND_HAS_SUBCOMMANDS` for subcommands that can never be
reached, `RAW_COMMAND_HAS_HANDLER` for a `.handler(...)` that can never run.

## Built-in `doctor` and self-audit

`AgentCli::doctor(checks)` registers a `doctor` command that runs your
[`Check`]s and reports `{ healthy, skipped, checks: [...] }`. A failing check
still produces an `ok: true` envelope (the report ran) but carries that check's
exit code (e.g. `ExitCode::AUTH`), so the shell sees a non-zero status.

A check has three outcomes. `CheckResult::pass()`, `CheckResult::fail(detail,
fix)`, and `CheckResult::skip(reason)` — the last for a check that never ran
because it does not apply: no bucket configured, an optional dependency absent.
Each check entry reports `status` (`"pass"` / `"fail"` / `"skip"`) next to `ok`,
and the report counts the skips. A skipped check leaves `healthy` true and never
contributes its exit code, so an optional subsystem cannot fail a `doctor` run
for a caller that does not use it — and an agent is never told a thing was
verified when nothing was.

### A `doctor` with your own flags

`doctor(checks)` hardcodes the command: named `doctor`, described "Run
environment health checks", usage `<cli> doctor`, no flags. When the command
needs to be yours — its own description, or a `--profile <name>` picking which
credentials to verify — build the `Command` and pass it to
`doctor_with(command, checks)`. agcli supplies only the handler that runs the
checks and builds the report; `doctor(checks)` is now sugar over it.

Declared flags reach the checks through `Check::with_request`, which hands the
check the same `CommandRequest` a handler gets:

```rust
let doctor = Command::new("doctor", "Check the active or named profile")
    .usage("app doctor [--profile=<name>]");

let cli = AgentCli::new("app", "…").doctor_with(doctor, vec![
    Check::with_request("profile", |req| {
        let profile = req.flag("profile").unwrap_or("active").to_string();
        Box::pin(async move {
            match load_profile(&profile).await {
                Some(p) => CheckResult::pass_with(format!("profile {p} resolves")),
                None => CheckResult::fail("no such profile", "Run `app login`"),
            }
        })
    })
    .exit_code(ExitCode::AUTH),
]);
```

Declaring the flag in the usage string is what makes it legal: unknown-flag
rejection reads that string as the flag schema, so `app --profile ci doctor`
fails with `UNKNOWN_FLAG` until `--profile` appears there. `Check::new` checks
still work unchanged and can sit in the same list.

`AgentCli::audit()` statically validates the command tree and returns an
`AuditReport`: it flags dangling `next_action` templates (HATEOAS integrity),
dead-end commands, unreachable subcommands under a raw command, and missing
usage/descriptions. Use it in a test:

```rust
assert!(cli.audit().is_clean());
```

## Bootstrap an agent

`AgentCli::skill()` registers a `skill` command that prints the CLI as an agent
skill — a `SKILL.md` file with YAML frontmatter, generated from the live command
tree. Help becomes a skill: an agent that can run the binary once learns the
whole surface, and the file cannot drift from the code because nobody writes it
by hand.

```bash
calc skill                            # markdown inside the envelope result
calc skill --install=.claude/skills   # writes .claude/skills/calc/SKILL.md
```

The document carries the envelope contract, every command usage template
(subcommands indented, raw commands marked), the reserved agent flags, and the
exit-code and error-code dictionaries — the same tables the root tree publishes.
The document is rendered when the command runs, from the finished tree, so
`.skill()` can sit anywhere in the builder chain. `cli.skill_markdown()`
returns the same string from your own code, which is what a test pins.

`--install` answers with the absolute `path` and its `bytes` instead of the
markdown. Under `--dry-run` it reports the path it would write and writes
nothing. `audit()` reports `SKILL_NAME_INVALID` when the CLI name is not a slug:
the name is both the frontmatter `name` and the directory written into.

## Performance

agcli targets **macOS and Linux only**. The crate ships with optimized release/bench profiles. To maximize runtime performance in a downstream binary:

### Recommended `Cargo.toml`

```toml
[dependencies]
agcli = "0.16.0"

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
- a raw passthrough command (`ops echo`) that owns its stdout and exit code
