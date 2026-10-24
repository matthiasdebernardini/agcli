# agcli

agcli is a Rust crate for building command-line tools that AI agents operate.
Every command answers with one JSON envelope: what happened, what went wrong
and how to fix it, and what the agent can run next. The tool documents itself,
so an agent can learn it from the binary alone.

You describe commands with a builder. agcli parses argv, checks it against each
command's usage string, runs your async handler, and prints the envelope. It
also gives you bounded lists, truncation with a file pointer, NDJSON
(newline-delimited JSON: one JSON object per line) streams, raw passthrough
commands, a static audit of the command tree, and built-in `doctor` and `skill`
commands.

## Principles

agcli follows five rules. They come from Joel Hooks's "Agent-First CLI Design"
skill; agcli makes them the default in Rust.

1. **JSON always.** Every command answers in JSON on stdout: one envelope, or
   for a streaming command one JSON object per line, ending with the envelope.
   No prose, no tables, no color codes: agents parse JSON reliably and prose
   badly. There is no `--json` opt-in and no human mode. A person can pipe the
   output through `jq`.
2. **HATEOAS** (Hypermedia as the Engine of Application State: each answer says
   what the agent can do next). Every envelope carries `next_actions`, command
   templates for the steps that make sense now, pre-filled from what just
   happened. The agent follows them instead of guessing.
3. **A self-documenting tree.** The program run with no arguments returns every
   command with its usage template, the agent flags, and the exit-code and
   error-code dictionaries. `help <command>` narrows it to one command, and
   `skill` renders the same tree as a `SKILL.md` file.
4. **Context protection.** Agents have finite context windows. Lists say how
   many rows exist and how to narrow them, and long output can be cut to its
   tail with a pointer to a file holding all of it. Both are opt-in: agcli never
   cuts a result on its own.
5. **Errors suggest fixes.** Every error carries a machine-readable `code`, a
   `fix` that says what to do, a `retryable` hint, and a typed exit code. An
   agent can recover without parsing the message.

A few commands owe their callers a different contract, such as a `grep` whose
output other tools parse. Raw commands opt out of the envelope for that case.

## Install

```bash
cargo add agcli serde_json
cargo add tokio --features macros,rt-multi-thread
```

agcli depends only on `serde` and `serde_json`. Handlers are futures, and any
executor can drive them. tokio is here only for the `#[tokio::main]` in the
examples.

The `deserialize` feature adds `Deserialize` to the wire types, for a program
that reads agcli output: `cargo add agcli --features deserialize`.

## A first CLI

This is the calculator in
[`examples/calc.rs`](https://github.com/matthiasdebernardini/agcli/blob/master/examples/calc.rs).
`cli()` builds the command tree. The lines after it run the CLI in-process, the
way a test does.

```rust
use agcli::{
    ActionParam, AgentCli, Command, CommandError, CommandOutput, Envelope, ErrorCode, ExitCode,
    NextAction,
};
use serde_json::json;

/// `serde_json` renders non-finite floats as `null`, which would corrupt the
/// result while still reporting `ok: true`, so overflow is a typed error.
fn finite(value: f64, operation: &str) -> Result<f64, CommandError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(CommandError::new(
            ErrorCode::new("OVERFLOW"),
            format!("{operation} overflowed the f64 range"),
            "Use smaller operands; f64 arithmetic saturates to infinity past ~1.8e308.",
        )
        .exit_code(ExitCode::USAGE))
    }
}

/// The calculator's command tree.
fn cli() -> AgentCli {
    AgentCli::new("calc", "Agent-native calculator")
        .version("1.0.0")
        .command(
            Command::new("add", "Add two numbers")
                .usage("calc add <a> <b>")
                .handler(|req| async move {
                    // The request is owned: the typed helpers turn a missing or
                    // unparseable argument into a MISSING_ARG / INVALID_ARG error.
                    let a: f64 = req.arg_parse(0, "a")?;
                    let b: f64 = req.arg_parse(1, "b")?;
                    let sum = finite(a + b, "add")?;
                    Ok(CommandOutput::new(json!({
                        "operation": "add",
                        "result": sum,
                    }))
                    .next_action(
                        NextAction::new("calc add <a> <b>", "Add two more numbers")
                            .with_param(
                                "a",
                                ActionParam::new()
                                    .value(json!(sum))
                                    .description("First number (pre-filled with previous result)"),
                            )
                            .with_param(
                                "b",
                                ActionParam::new()
                                    .required(true)
                                    .description("Second number"),
                            ),
                    )
                    .next_action(
                        NextAction::new("calc sub <a> <b>", "Subtract instead")
                            .with_param("a", ActionParam::new().value(json!(sum)))
                            .with_param("b", ActionParam::new().required(true)),
                    ))
                }),
        )
        .command(
            Command::new("sub", "Subtract two numbers")
                .usage("calc sub <a> <b>")
                .handler(|req| async move {
                    let a: f64 = req.arg_parse(0, "a")?;
                    let b: f64 = req.arg_parse(1, "b")?;
                    let diff = finite(a - b, "sub")?;
                    Ok(CommandOutput::new(json!({
                        "operation": "sub",
                        "result": diff,
                    }))
                    .next_action(
                        NextAction::new("calc sub <a> <b>", "Subtract two more numbers")
                            .with_param("a", ActionParam::new().value(json!(diff)))
                            .with_param("b", ActionParam::new().required(true)),
                    ))
                }),
        )
        .skill()
}
# #[tokio::main(flavor = "current_thread")]
# async fn main() {

// In-process: no argv, no printing, no exit. This is how a test drives it.
let run = cli().run(["calc", "add", "3", "5"]).await;
assert_eq!(run.exit_code(), ExitCode::SUCCESS);
let Envelope::Success(answer) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(answer.result, json!({ "operation": "add", "result": 8.0 }));

let run = cli().run(["calc", "add", "foo", "bar"]).await;
assert_eq!(run.exit_code(), ExitCode::USAGE);
let Envelope::Error(failure) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(failure.error.code, ErrorCode::INVALID_ARG);
# }
```

The binary's `main` hands the process to the CLI. `run_env` reads the real
argv, and `finish` prints the answer and exits with its exit code:

```rust,no_run
# fn cli() -> agcli::AgentCli { agcli::AgentCli::new("calc", "Agent-native calculator") }
#[tokio::main]
async fn main() {
    cli().run_env().await.finish()
}
```

`finish` ends in `std::process::exit`, which runs no destructors. A `main` that
needs them returns `std::process::ExitCode` instead:
`cli().run_env().await.print().into()`.

### Run it

```bash
cargo run --example calc                 # the command tree
cargo run --example calc -- add 3 5      # a result
cargo run --example calc -- add foo bar  # a typed error, exit code 2
cargo run --example calc -- skill        # the CLI as a SKILL.md
```

`calc add 3 5` prints this, on one line (indented here):

```json
{
  "ok": true,
  "command": "calc add 3 5",
  "timestamp": "2026-06-10T14:42:17Z",
  "exit_code": 0,
  "result": { "operation": "add", "result": 8.0 },
  "next_actions": [
    {
      "command": "calc add <a> <b>",
      "description": "Add two more numbers",
      "params": {
        "a": { "description": "First number (pre-filled with previous result)", "value": 8.0 },
        "b": { "description": "Second number", "required": true }
      }
    },
    {
      "command": "calc sub <a> <b>",
      "description": "Subtract instead",
      "params": { "a": { "value": 8.0 }, "b": { "required": true } }
    }
  ]
}
```

The handler pre-fills `a` with the sum, so the agent's next step needs one
number. `calc add foo bar` fails in `arg_parse`:

```json
{
  "ok": false,
  "command": "calc add foo bar",
  "timestamp": "2026-06-10T14:42:17Z",
  "exit_code": 2,
  "error": {
    "message": "argument <a> is not valid: \"foo\" (invalid float literal)",
    "code": "INVALID_ARG",
    "retryable": false
  },
  "fix": "Pass a valid value for <a>.",
  "next_actions": [
    {
      "command": "calc add <a> <b>",
      "description": "Run this command template",
      "params": { "a": { "required": true }, "b": { "required": true } }
    },
    { "command": "calc", "description": "Inspect the full command tree" }
  ]
}
```

The error came with no `next_actions`, so the framework added the command's own
template and the root. The exit code is 2 (usage), in the JSON and as the
process status.

## Key patterns

### Handlers and typed arguments

A handler is an async closure. It takes the `CommandRequest` by value and
returns `Result<CommandOutput, CommandError>`, so `?` works on anything that
fails with a `CommandError`.

The typed accessors turn the usual failures into errors with a conventional
code, a generated `fix`, and exit code 2:

| Accessor | Returns | Fails with |
|----------|---------|------------|
| `req.arg(i)` | `Option<&str>` | never |
| `req.require_arg(i, "name")` | `&str` | `MISSING_ARG` |
| `req.arg_parse::<T>(i, "name")` | `T` | `MISSING_ARG`, `INVALID_ARG` |
| `req.flag("key")` | `Option<&str>` | never |
| `req.flag_parse::<T>("key")` | `Option<T>` | `INVALID_FLAG` |
| `req.flag_on("key")` | `bool` | `INVALID_FLAG` |

`arg_parse` and `flag_parse` also refuse `inf` and `NaN`, which `serde_json`
would print as `null` under `ok: true`. `flag_on` reads a boolean strictly:
absent is `false`, a bare `--force` is `true`, and a value must be one of
`true`/`false`, `1`/`0`, `yes`/`no` or `on`/`off`. Reading
`req.flag("force").is_some()` instead would turn `--force=false` on.

```rust
use agcli::{AgentCli, Command, CommandOutput, Envelope, ErrorCode, ExitCode};
use serde_json::json;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let cli = AgentCli::new("ops", "Agent-native operations CLI").command(
    Command::new("logs", "Show recent log lines")
        .usage("ops logs <source> [--lines=<n>] [--follow]")
        .handler(|req| async move {
            let source = req.require_arg(0, "source")?;
            let lines = req.flag_parse::<usize>("lines")?.unwrap_or(20);
            let follow = req.flag_on("follow")?;
            Ok(CommandOutput::new(json!({ "source": source, "lines": lines, "follow": follow })))
        }),
);

let run = cli.run(["ops", "logs", "worker", "--lines", "5", "--follow"]).await;
let Envelope::Success(answer) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(answer.result, json!({ "source": "worker", "lines": 5, "follow": true }));

let run = cli.run(["ops", "logs", "worker", "--lines=many"]).await;
assert_eq!(run.exit_code(), ExitCode::USAGE);
let Envelope::Error(failure) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(failure.error.code, ErrorCode::INVALID_FLAG);
assert_eq!(failure.fix, "Pass a valid value for --lines.");
# }
```

The request also gives `req.positionals()` (the command's own arguments),
`req.command_path()` (`["db", "migrate"]`), `req.invocation()` (the whole
parsed line), `req.now()` (the run's clock), and typed reads of the agent
flags, listed under Agent flags below.

A handler is `Fn`, so it may run more than once. State it owns reaches the
future by a clone per call, not by a move out of the closure:

```rust
use std::sync::{Arc, Mutex};

use agcli::{AgentCli, Command, CommandOutput, Envelope};
use serde_json::json;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let notes: Arc<Mutex<Vec<String>>> = Arc::default();
let store = Arc::clone(&notes);
let cli = AgentCli::new("notes", "Agent-native notes").command(
    Command::new("add", "Add a note")
        .usage("notes add <text>")
        .handler(move |req| {
            let store = Arc::clone(&store);
            async move {
                let text = req.require_arg(0, "text")?.to_string();
                let mut store = store.lock().expect("not poisoned");
                store.push(text);
                Ok(CommandOutput::new(json!({ "count": store.len() })))
            }
        }),
);

let run = cli.run(["notes", "add", "milk"]).await;
let Envelope::Success(answer) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(answer.result, json!({ "count": 1 }));
assert_eq!(*notes.lock().expect("not poisoned"), ["milk"]);
# }
```

`CommandOutput::try_from_serializable(value)` builds a result from any
`Serialize` type, and returns the `serde_json::Error` when the value cannot
become JSON. `CommandOutput::exit_code(...)` reports success with a non-zero
status, the way `doctor` reports an unhealthy system.

### The usage string is the schema

`Command::usage` is documentation, and it is also the rules the framework
enforces before the handler runs:

- `<name>` is a required positional and `[<name>]` (or `[name]`) an optional
  one. Surplus positionals are refused with `EXTRA_ARG`, and nothing runs: a
  silently dropped argument is how `tool delete <id>` given two ids deletes the
  wrong thing. A variadic tail (`[args...]`) lifts the limit, and so does
  `Command::allow_extra_args()`.
- `--flag=<v>`, `--flag <v>` and `-n <count>`, bracketed or not, declare value
  flags. `[--flag]` and `[-v]` declare booleans, which never take the next
  token as their value.
- A flag that no usage string on the command's path declares, and that the
  framework does not reserve, is refused with `UNKNOWN_FLAG`. The `fix` carries
  a did-you-mean hint and the valid flags. `Command::allow_unknown_flags()`
  turns this off for a command that forwards flags to another program.
- Each placeholder becomes a `params` entry in the `next_actions` the framework
  writes from the usage: required for `<name>`, optional inside brackets.

A command without a usage string gets no flag or arity checks, and the tree
shows a generic template for it. A group without one declares no flags, so a
flag the framework does not reserve is `UNKNOWN_FLAG` there, as on the root.
Start every usage string with the program name, since agents copy templates as
printed, and make its command tokens name the command itself: a usage for `get`
that reads `app fetch <id>` is a template agents copy into `UNKNOWN_COMMAND`.
Keep the brackets balanced, and do not declare `--help`, `-h`, `--version` or
`-V`, which are the framework's own. `audit()` reports each of these mistakes,
the path mismatch as `USAGE_PATH_MISMATCH`.

### How argv is parsed

- `--key=value` and `--key value` mean the same, except for booleans. A
  declared boolean, a reserved agent flag, `--help` and `--version` never take
  the next token.
- Any other bare `--key` takes the next token as its value when that token is
  not a flag. To keep a positional after an undeclared boolean, write
  `--key=true` or put `--` before the positionals.
- `-x`, `-x=value` and `-x value` are short flags. `-abc` sets three booleans.
  `-abc=5` cannot say which flag the value belongs to, so it is a
  `PARSE_ERROR`.
- `-3`, `-2.5`, and any `-` followed by a digit (`-0x10`, `-1,000`) are
  positionals, not flags.
- `--` ends flag parsing: every later token is a positional.
- A flag given twice, in any spelling (`--n=1 -n 2`, `-vv`), is a
  `PARSE_ERROR` such as `-n given twice`. The parser never picks a winner.
- Each `PARSE_ERROR` carries the `fix` for its cause. A repeated `--select`,
  for example, says to pass every field in one comma-separated list.
- Booleans are scoped to the resolved command, so two commands can use one flag
  name with different meanings.

### `next_actions`

A `next_action` is a command and a description:

- With `params`, `command` is a template. Fill every placeholder before running
  it. `<name>` is required and `[--flag <value>]` is optional.
- Without `params`, run `command` as printed.
- In a param, `value` is pre-filled from what just happened (the agent may
  override it), `default` applies when the agent leaves it out, `enum` lists the
  valid choices, and `required` says whether it must be given.

```rust
use agcli::{ActionParam, NextAction};
use serde_json::json;

let logs = NextAction::new("ops logs <source> [--lines=<lines>]", "Inspect logs")
    .with_param(
        "source",
        ActionParam::new()
            .enum_values(["worker", "errors", "server"])
            .default_value("worker"),
    )
    .with_param(
        "lines",
        ActionParam::new().description("Number of lines to show").default_value(20),
    );

assert_eq!(
    serde_json::to_value(&logs).expect("a next_action serializes"),
    json!({
        "command": "ops logs <source> [--lines=<lines>]",
        "description": "Inspect logs",
        "params": {
            "source": { "default": "worker", "enum": ["worker", "errors", "server"] },
            "lines": { "description": "Number of lines to show", "default": 20 }
        }
    })
);
```

`next_actions` should follow from what just happened: a failure suggests
different steps than a success. When a handler returns none, the framework adds
the command's `default_next_action`s, or else the command's own template and
the root command. Errors get the same treatment. A list result gets one more
action, described under Lists.

The framework writes its own actions from usage strings: every `<placeholder>`
becomes a param (required when bare, optional inside brackets, a variadic
`<args...>` included), and a usage with no placeholder becomes a literal whose
optional `[...]` parts are dropped so it runs as printed: `ops echo [args...]`
is offered as `ops echo`.

### Errors

`CommandError::new(code, message, fix)` takes the code first, as its own type,
so the three cannot trade places. The fix is required: every error tells the
agent how to recover.

- `ErrorCode::new("NOT_FOUND")` names a code of your own, and
  `ErrorCode::owned(format!("HTTP_{status}"))` one built at run time. The
  framework's codes are constants, such as `ErrorCode::INVALID_ARG`. A code is
  never empty.
- `.exit_code(ExitCode::NOT_FOUND)` picks the process status. The default is 1.
  `ExitCode::SUCCESS` panics, because an error that exits 0 reads as success.
- `.retryable(true)` marks a transient failure that the same call may survive.
- `.data(json!(...))` attaches facts the agent should act on, such as the
  rejected rows and why, so nobody parses them back out of the message.
- `.next_action(...)` points at the recovery.

```rust
use agcli::{
    AgentCli, Command, CommandError, CommandOutput, Envelope, ErrorCode, ExitCode, NextAction,
};
use serde_json::json;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let cli = AgentCli::new("app", "Issue tracker")
    .command(
        Command::new("ls", "List the issues")
            .usage("app ls")
            .handler(|_req| async move { Ok(CommandOutput::list(vec![json!({ "id": 7 })])) }),
    )
    .command(
        Command::new("get", "Show one issue")
            .usage("app get <id>")
            .handler(|req| async move {
                let id: u64 = req.arg_parse(0, "id")?;
                if id != 7 {
                    return Err(CommandError::new(
                        ErrorCode::new("NOT_FOUND"),
                        format!("no issue {id}"),
                        "List the issues with `app ls` and pick an existing id.",
                    )
                    .exit_code(ExitCode::NOT_FOUND)
                    .data(json!({ "id": id }))
                    .next_action(NextAction::new("app ls", "List the issues")));
                }
                Ok(CommandOutput::new(json!({ "id": id, "title": "Fix the login page" })))
            }),
    );

let run = cli.run(["app", "get", "9"]).await;
assert_eq!(run.exit_code(), ExitCode::NOT_FOUND);
let Envelope::Error(failure) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(failure.error.code, ErrorCode::new("NOT_FOUND"));
assert_eq!(failure.next_actions[0].command, "app ls");
assert!(cli.audit().is_clean());
# }
```

`app get 9` prints:

```json
{
  "ok": false,
  "command": "app get 9",
  "timestamp": "2026-06-10T14:42:17Z",
  "exit_code": 3,
  "error": { "message": "no issue 9", "code": "NOT_FOUND", "retryable": false },
  "fix": "List the issues with `app ls` and pick an existing id.",
  "data": { "id": 9 },
  "next_actions": [{ "command": "app ls", "description": "List the issues" }]
}
```

The exit codes and the framework's error codes are listed under Wire contract.

### Subcommands and the command tree

```rust
use agcli::{AgentCli, Command, CommandOutput, Envelope};
use serde_json::json;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let cli = AgentCli::new("app", "Example CLI").version("2.1.0").command(
    Command::new("db", "Database commands").subcommand(
        Command::new("migrate", "Run pending migrations")
            .usage("app db migrate")
            .handler(|_req| async move { Ok(CommandOutput::new(json!({ "applied": 0 }))) }),
    ),
);

// The bare program answers with the whole tree.
let run = cli.run(["app"]).await;
let Envelope::Success(root) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(root.result["commands"][0]["subcommands"][0]["usage"], "app db migrate");

// A group without a handler, `--help` and `help` all answer with a subtree.
for argv in [vec!["app", "db"], vec!["app", "db", "--help"], vec!["app", "help", "db"]] {
    let run = cli.run(argv).await;
    let Envelope::Success(tree) = run.envelope() else { panic!("{}", run.to_json()) };
    assert_eq!(tree.result["subcommands"][0]["name"], "migrate");
}

// `--version` answers wherever it sits.
let run = cli.run(["app", "db", "migrate", "--version"]).await;
let Envelope::Success(version) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(version.result, json!({ "name": "app", "version": "2.1.0" }));
# }
```

`Command::subcommand` nests commands. A group without a handler answers with
its own subtree, and an unknown name under it is `UNKNOWN_SUBCOMMAND` with the
valid names in `fix`. An unknown command is `UNKNOWN_COMMAND` the same way, with
"Did you mean `status`?" in front when the token looks like a typo (an edit
distance of at most 2, and less than the name's length). Registering two
commands with the same name panics, as does a mistake in any other builder
call: an error with exit code 0, a list whose `total` is below its rows, a
`skill()` on a name that is not a slug.

The framework answers these itself:

- **The bare program**: the root tree. Its `result` holds `description`,
  `version` (when set), `commands`, `agent_flags`, `exit_codes` and
  `error_codes`. Each command entry has `name`, `description` and `usage`, plus
  `subcommands`, `"raw": true` or `"stream": true` where they apply.
  `AgentCli::root_field(key, value)` adds a key of your own, such as live
  health. It cannot replace the framework's keys.
- **`--help` or `-h` anywhere, and `help <command...>`**: the named command's
  `name`, `description`, `usage` and `subcommands`.
- **`--version` or `-V` anywhere, and `version` alone**: `name` and `version`.
  The positionals must name a command or nothing: `app nope --version` is
  `UNKNOWN_COMMAND` for `nope`, as it is with `--help`, and `app version extra`
  is `EXTRA_ARG`. Help wins when both are asked for.

The `help` and `version` words yield to commands of those names that the CLI
defines. The flags never do.

These answers check flags the way the named command would, and they honor
`--select`, `--compact` and `--quiet`: `app --select=commands` returns only the
command list. A line that names a raw command goes to that command with every
token after its name; the tokens before the name are the framework's (see
[Raw commands](#raw-commands)).

### Agent flags

Every command accepts these flags without declaring them:

| Flag | What happens |
|------|--------------|
| `--select=<a,b,c>` | The result is projected to these top-level keys or `a.b` dot paths, mapped over arrays. A miss is reported in `select_warning`, below. |
| `--compact` | `null` values and empty containers are dropped from the result, or only the command's `CommandOutput::compact_fields` are kept. Beside `--select` it only drops nulls and empty containers from the selection; when that would empty the selection, the selection is returned uncompacted with a `select_warning`. |
| `--quiet` | A successful answer has no `next_actions`. Errors keep theirs: they are how the agent recovers. |
| `--dry-run` | Refused with `DRY_RUN_UNSUPPORTED` unless the command declares `handles_dry_run()`. The handler reads `req.dry_run()`. |
| `--yes`, `--no-input` | Read by the handler with `req.assume_yes()`. |
| `--no-cache` | Read with `req.no_cache()`. |
| `--no-color` | Read with `req.no_color()`. |
| `--stdin` | Read with `req.wants_stdin()`. The handler reads stdin itself, for example with `std::io::read_to_string(std::io::stdin())`. |
| `--json` | Accepted and ignored. |

```rust
use agcli::{AgentCli, Command, CommandOutput, Envelope, ErrorCode};
use serde_json::json;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let cli = AgentCli::new("app", "Issue tracker")
    .command(
        Command::new("get", "Show one issue")
            .usage("app get <id>")
            .handler(|_req| async move {
                Ok(CommandOutput::new(json!({
                    "id": 7,
                    "title": "Fix the login page",
                    "assignee": null,
                    "labels": [],
                })))
            }),
    )
    .command(
        Command::new("close", "Close an issue")
            .usage("app close <id>")
            .handles_dry_run()
            .handler(|req| async move {
                let id = req.require_arg(0, "id")?;
                Ok(CommandOutput::new(json!({ "id": id, "closed": !req.dry_run() })))
            }),
    );

let run = cli.run(["app", "get", "7", "--select=id,title"]).await;
let Envelope::Success(answer) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(answer.result, json!({ "id": 7, "title": "Fix the login page" }));

// A select that matches nothing keeps the result and says why.
let run = cli.run(["app", "get", "7", "--select=titel"]).await;
let Envelope::Success(answer) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(answer.result["title"], "Fix the login page");
assert!(answer.select_warning.is_some());

let run = cli.run(["app", "get", "7", "--compact", "--quiet"]).await;
let Envelope::Success(answer) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(answer.result, json!({ "id": 7, "title": "Fix the login page" }));
assert!(answer.next_actions.is_empty());

// --dry-run reaches only a command that declares it.
let run = cli.run(["app", "close", "7", "--dry-run"]).await;
let Envelope::Success(answer) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(answer.result["closed"], false);
let run = cli.run(["app", "get", "7", "--dry-run"]).await;
let Envelope::Error(refused) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(refused.error.code, ErrorCode::DRY_RUN_UNSUPPORTED);
# }
```

- **`--select` never wipes a result.** A `--select` with no field names, one
  that matches nothing, or one on a result that is not an object or an array
  of objects, returns the full result plus a `select_warning`. For a miss, the
  warning lists the fields that exist: the keys of an object, or every key
  found across the rows of an array. A partial miss keeps the projection and
  names the dropped fields, and a dot path that reaches nothing (`a.zzz`,
  `items.zzz`) is a miss like any other. The warning is a key of the envelope,
  beside `result`, so it never collides with the handler's data:

  ```json
  {
    "ok": true,
    "command": "app get 7 --select=titel",
    "timestamp": "2026-06-10T14:42:17Z",
    "exit_code": 0,
    "result": { "assignee": null, "id": 7, "labels": [], "title": "Fix the login page" },
    "select_warning": "--select=titel matched no fields. Available top-level fields: assignee, id, labels, title. Returning the full result; re-run --select with a valid field name.",
    "next_actions": [
      { "command": "app get <id>", "description": "Run this command template", "params": { "id": { "required": true } } },
      { "command": "app", "description": "Inspect the full command tree" }
    ]
  }
  ```

- **`--dry-run` is a promise**: preview, change nothing. The framework cannot
  know whether a handler keeps it, so it refuses the flag, and runs nothing,
  unless the command or one of its parents declares `handles_dry_run()`.
- **Before a raw command's name they are refused**, because a raw handler
  receives only the tokens after its name: `--dry-run` is
  `DRY_RUN_UNSUPPORTED`, and the rest but `--json` are `INVALID_FLAG` (see
  [Raw commands](#raw-commands)).
- **`--select` and `--compact` never touch the `doctor` report**, so a
  projection cannot hide a failing check's fix.
- **The booleans take only boolean spellings**: `true`/`false`, `1`/`0`,
  `yes`/`no`, `on`/`off`, in any ASCII case. A bare flag is `true`, and
  `--quiet=maybe` is `INVALID_FLAG`.
- **`--json` is reserved** because it is the flag an agent most often guesses on
  an unfamiliar tool, and the output is JSON anyway. As a boolean it never takes
  the next token: `app brain --json myrepo` keeps `myrepo` as a positional.
- **They are discoverable.** The root tree lists them under `agent_flags`, and
  `agcli::reserved_flag_names()` returns their names.
- **`AgentCli::without_reserved_flags()`** turns them off, for a CLI whose
  commands need these names for something else. With them off, the framework
  also stops checking unknown flags, surplus positionals and `--dry-run`, and
  projects nothing. `--help`, `-h`, `--version` and `-V` stay reserved.

### Lists

```rust
use agcli::{AgentCli, Command, CommandOutput, Envelope};
use serde_json::json;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let cli = AgentCli::new("notes", "Agent-native notes").command(
    Command::new("ls", "List notes")
        .usage("notes ls")
        .handler(|_req| async move {
            Ok(CommandOutput::list(vec![
                json!({ "id": 1, "title": "first", "done": false }),
                json!({ "id": 2, "title": "second", "done": true }),
            ]))
        }),
);

let run = cli.run(["notes", "ls"]).await;
let Envelope::Success(list) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(list.result["fields"], json!(["items.done", "items.id", "items.title"]));
let projected = list.next_actions.last().expect("the --select advertisement");
assert_eq!(projected.command, "notes ls --select=<fields>");

// Pasted back as advertised, the paths project every row.
let run = cli.run(["notes", "ls", "--select=items.id,items.title"]).await;
let Envelope::Success(list) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(
    list.result,
    json!({ "items": [{ "id": 1, "title": "first" }, { "id": 2, "title": "second" }] })
);
# }
```

`notes ls` prints:

```json
{
  "ok": true,
  "command": "notes ls",
  "timestamp": "2026-06-10T14:42:17Z",
  "exit_code": 0,
  "result": {
    "count": 2,
    "fields": ["items.done", "items.id", "items.title"],
    "items": [
      { "done": false, "id": 1, "title": "first" },
      { "done": true, "id": 2, "title": "second" }
    ],
    "total": 2,
    "truncated": false
  },
  "next_actions": [
    { "command": "notes ls", "description": "Run this command template" },
    { "command": "notes", "description": "Inspect the full command tree" },
    {
      "command": "notes ls --select=<fields>",
      "description": "Re-run projected to only the fields you need: a smaller result, the same data",
      "params": {
        "fields": {
          "description": "Comma-separated subset of: items.done, items.id, items.title (dot paths project each row)",
          "required": true
        }
      }
    }
  ]
}
```

`CommandOutput::list(items)` and `CommandOutput::list_truncated(items, total)`
return `items`, `count`, `total` and `truncated`. When `total` is more than the
rows returned, a `guidance` string says how to narrow the query; a `total`
below the rows returned is a mistake, and `list_truncated` panics on it.

For object rows, `fields` lists the `--select` paths that cover a row: one
sorted `items.<key>` for each key found across the rows that `--select` can
spell (a key holding `.` or `,` cannot be named, so it is left out). The paths
are dot paths because `--select` projects top-level keys, and a bare `id`
would miss the rows nested under `items`. The framework then appends a
`next_action` that reruns this exact invocation under `--select`, with the flag
before any `--`; it is left out when `--select` is already on the line, under
`--quiet`, and when the agent flags are off.

### Long output

```rust
use agcli::truncate_lines_with_file;

let lines: Vec<String> = (0..120).map(|n| format!("[worker] line-{n}")).collect();
let tail = truncate_lines_with_file(lines, 20, "ops-logs").expect("the temp dir is writable");
assert!(tail.truncated);
assert_eq!((tail.lines, tail.total, tail.dropped), (20, 120, 100));
assert_eq!(tail.entries[0], "[worker] line-100");

// The full output stays on disk for the agent to read. The caller removes it.
let path = tail.full_output.clone().expect("a cut result points at the full output");
assert!(std::path::Path::new(&path).exists());
tail.cleanup().expect("the file is ours to remove");
```

`truncate_lines_with_file(lines, max_lines, file_prefix)` keeps the last
`max_lines` lines, because logs want the most recent ones. When it cuts
anything, it writes every line to `<file_prefix>-<pid>-<nanos>.log` in the
system temp directory, readable only by the owner. The result reports `lines`,
`total`, `dropped` (cut from the head), `truncated`, `full_output` and
`entries`. Put it in a result with `CommandOutput::new(json!(tail))`.

The file outlives the command so the agent can follow the pointer. The caller
removes it with `TruncatedEntries::cleanup()`. A `max_lines` of 0, or a
`file_prefix` that is not ASCII letters, digits, `-` and `_`, is an
`io::ErrorKind::InvalidInput` error. Validate an agent-supplied count first,
for example with `req.flag_parse::<NonZeroUsize>("lines")`, so the agent gets an
error that names its flag. `ops logs` in
[`examples/ops.rs`](https://github.com/matthiasdebernardini/agcli/blob/master/examples/ops.rs)
shows the whole pattern.

### Streaming commands

An envelope says what the state is now. A stream says what is happening over
time: a deploy, a watch, a tail. Stream when the command is about time, and
keep point-in-time queries as single envelopes. The command decides; there is
no global `--stream` flag.

```rust
use agcli::{AgentCli, Command, CommandOutput, StepStatus, StreamEvent};
use serde_json::json;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let cli = AgentCli::new("ops", "Agent-native operations CLI").command(
    Command::new("deploy", "Deploy a service, streaming each step")
        .usage("ops deploy <service>")
        .stream_handler(|req, out| async move {
            let service = req.require_arg(0, "service")?.to_string();
            for step in ["build", "upload"] {
                out.emit(&StreamEvent::Step {
                    name: step.to_string(),
                    status: StepStatus::Completed,
                    duration_ms: Some(0),
                    error: None,
                })?;
            }
            Ok(CommandOutput::new(json!({ "service": service, "deployed": true })))
        }),
);

// In-process, the events are captured instead of printed.
let run = cli.run(["ops", "deploy", "web"]).await;
assert!(run.is_stream());
assert_eq!(run.stream_events().len(), 2);
assert!(run.stream_events()[0].starts_with(r#"{"type":"step","name":"build","status":"completed""#));
// `to_json` is the terminal line: the envelope, led by its `type`.
assert!(run.to_json().starts_with(r#"{"type":"result","ok":true,"command":"ops deploy web""#));
# }
```

`Command::stream_handler` hands the handler a `StreamWriter`. Each
`out.emit(&event)` writes one NDJSON line, stamped with `ts` from the run's
clock, and flushes it. The writer is `Clone`, so a task the handler spawns can
emit through a clone: lines are written whole, one at a time. The handler
returns the same `Result` a normal handler does. From it the framework writes
exactly one terminal line: the envelope, with `"type":"result"` or
`"type":"error"` in front. `--select`, `--quiet` and `next_actions` apply to that
line as to any envelope.

| `type` | Keys | Terminal |
|--------|------|----------|
| `start` | `command` | no |
| `step` | `name`, `status` (`started`, `completed` or `failed`), `duration_ms`, `error` | no |
| `progress` | `name`, `percent`, `message` | no |
| `log` | `level` (`info`, `warn` or `error`), `message` | no |
| `event` | `name`, `data` | no |
| `result` | the success envelope | yes, always last |
| `error` | the error envelope | yes, always last |

Every event ends with `ts`. `ops deploy web` from `examples/ops.rs` prints:

```text
{"type":"start","command":"ops deploy web","ts":"2026-06-10T14:42:17Z"}
{"type":"step","name":"build","status":"started","ts":"2026-06-10T14:42:17Z"}
{"type":"step","name":"build","status":"completed","duration_ms":0,"ts":"2026-06-10T14:42:17Z"}
{"type":"progress","name":"deploy","percent":33,"message":"build done","ts":"2026-06-10T14:42:17Z"}
…
{"type":"result","ok":true,"command":"ops deploy web","timestamp":"2026-06-10T14:42:18Z","exit_code":0,"result":{"deployed":true,"service":"web","steps":["build","upload","activate"]},"next_actions":[{"command":"ops status","description":"Check health after the deploy"}]}
```

An agent reads the lines as they arrive and can act early: stop on a failed
step, report progress, and skip the polling. A tool that does not understand
streams reads the last line and gets the envelope it expects. Every line is
JSON, so `jq --unbuffered 'select(.type == "step")'` filters a live stream.

- A write that fails, because the reader went away, is a `StreamWriteError`.
  `?` turns it into a `STREAM_WRITE_FAILED` error. After a failed write the
  writer refuses every later one, so nothing lands after a partial line. It also
  closes when the handler returns, so nothing can follow the terminal line, not
  even through a clone a spawned task kept.
- Under `run_env` the events reach stdout as they are written. Under `run` they
  are captured: `Execution::stream_events()` returns them and `to_json()`
  returns the terminal line.
- A usage error on a streaming command, such as an unknown flag, a surplus
  argument, an unsupported `--dry-run` or a non-boolean value, is also a
  terminal `"type":"error"` line, so once the command runs, every line carries
  `type`. The help and version answers for a streaming command
  (`ops deploy --help`) are plain envelopes, like the framework's other
  answers.
- Streaming commands are marked `"stream": true` in the tree and in help, and
  the generated skill says they stream.

### Raw commands

Most commands should answer with an envelope. A few cannot: a `grep` that owes
its callers `path:line:content` and exit code 1 for "no matches", a `cat`, a
shim around another program. Wrapping those in JSON does not make them
agent-native; it makes them wrong. `Command::raw_handler` is the opt-out. The
handler gets every token after its command's name and returns the process exit
code.

```rust
use std::sync::{Arc, Mutex};

use agcli::{AgentCli, Command, ExitCode};

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let seen: Arc<Mutex<Vec<String>>> = Arc::default();
let record = Arc::clone(&seen);
let cli = AgentCli::new("app", "Example CLI").command(
    Command::new("grep", "Search files (ripgrep-compatible output)")
        .usage("app grep [rg-flags...] <pattern> [path...]")
        .raw_handler(move |args| {
            let record = Arc::clone(&record);
            async move {
                // A real handler prints `path:line:text` hits on stdout here.
                let no_match = args.is_empty();
                *record.lock().expect("not poisoned") = args;
                ExitCode::new(u8::from(no_match)) // ripgrep's convention: 1 means no match
            }
        }),
);

let run = cli.run(["app", "--json", "grep", "-C", "3", "--help", "--", "-x"]).await;
assert!(run.is_raw());
assert_eq!(*seen.lock().expect("not poisoned"), ["-C", "3", "--help", "--", "-x"]);
# }
```

What the framework skips for a raw command, and why that matters:

| Skipped | Why |
|---------|-----|
| Flag parsing | `-C 3`, `-t rust`, `-g '*.rs'`, `--` and patterns that start with `-` arrive untouched and in order. |
| Unknown-flag rejection | An unknown flag is the handler's business. |
| Positional checks | No `EXTRA_ARG` on a long argument list. |
| The `--dry-run` gate | After the name, the token is passed through like any other. Before the name it is `DRY_RUN_UNSUPPORTED`, below. |
| `--select`, `--compact`, `--quiet` | There is no envelope to project. Before the name they are `INVALID_FLAG`, below. |
| The envelope and `next_actions` | Stdout carries exactly what the handler printed. |

The command still appears in the tree (marked `"raw": true`), in help, and in
`audit()`.

- The handover holds wherever the name sits. `app grep -h pat`,
  `app --json grep -h pat`, and even `app --json grep --=x` (a line the parser
  rejects) all reach the handler. `--help`, `-h`, `--version` and `-V` after the
  name belong to the command. Before the name, the tokens are the framework's:
  `app --help grep` answers help for `grep` (as `app help grep` does),
  `app -V grep` the version, `app --quiet=maybe grep pat` is `INVALID_FLAG`,
  `app --=x grep pat` is `PARSE_ERROR`, and `app --bogus=1 grep pat` is
  `UNKNOWN_FLAG`.
- A raw handler receives only the tokens after its name, so a reserved agent
  flag before the name could reach nothing, and is refused instead of dropped.
  Nothing runs. `app --dry-run grep pat` is `DRY_RUN_UNSUPPORTED`: if `grep`
  has a preview flag of its own, pass that after its name. Every other
  reserved flag there (`--select`, `--compact`, `--quiet`, `--yes`,
  `--no-input`, `--no-cache`, `--no-color`, `--stdin`) is `INVALID_FLAG`:
  drop it, or pass it after the name if the command takes it. Only `--json`,
  which changes nothing anywhere, `--help` and `--version` are accepted
  before the name. Under `without_reserved_flags()`, no flag there is
  refused.
- One shape cannot work. A flag before the name that the framework does not
  know as a boolean takes the next token as its value, so
  `app --profile grep pat` reads `grep` as the value of `--profile` and names no
  raw command. Put such flags after the name, where they are the command's.
- To forward a child process's status, convert `status.code()` with
  `ExitCode::try_from`. It is `None` when a signal killed the child; what that
  means is the command's to decide.
- End `main` with `finish()` or `print()`, which print nothing after a raw
  command. A hand-rolled `println!("{}", run.to_json())` would append an
  envelope to the command's output. `finish()` runs no destructors, so a raw
  handler that buffers its own writer must flush before it returns. `println!`
  flushes each line.
- A raw command is a leaf. `audit()` reports subcommands declared under it.
- A command has one handler slot. `handler`, `stream_handler` and `raw_handler`
  replace each other, and the last call wins.

### doctor

```rust
use agcli::{AgentCli, Check, CheckResult, Command, Envelope, ExitCode};

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let cli = AgentCli::new("app", "Example CLI").doctor_with(
    Command::new("doctor", "Check the active or a named profile")
        .usage("app doctor [--profile=<name>]"),
    vec![
        Check::new("profile", |req| async move {
            match req.flag("profile") {
                None | Some("default") => CheckResult::pass_with("profile default resolves"),
                Some(name) => CheckResult::fail(format!("no profile {name}"), "Run `app login`"),
            }
        })
        .exit_code(ExitCode::AUTH),
        Check::new("s3", |_req| async { CheckResult::skip("no bucket configured") }),
    ],
);

let run = cli.run(["app", "doctor", "--profile=ci"]).await;
assert_eq!(run.exit_code(), ExitCode::AUTH); // the failing check's code
let Envelope::Success(report) = run.envelope() else { panic!("{}", run.to_json()) };
assert_eq!(report.result["healthy"], false);
assert_eq!(report.result["skipped"], 1);
assert_eq!(report.result["checks"][0]["status"], "fail");
# }
```

`app doctor --profile=ci` answers `"ok": true` with `"exit_code": 4` and this
result:

```json
{
  "checks": [
    { "detail": "no profile ci", "fix": "Run `app login`", "name": "profile", "status": "fail" },
    { "detail": "no bucket configured", "name": "s3", "status": "skip" }
  ],
  "healthy": false,
  "skipped": 1
}
```

`AgentCli::doctor(checks)` registers a fixed `doctor` command, described as
"Run environment health checks", with no flags. `doctor_with(command, checks)`
takes the `Command` from you: its name, description and usage string, so
`doctor` can declare flags such as `[--profile=<name>]` for its checks to read.
Each `Check::new` gets the same `CommandRequest` a handler gets. agcli supplies
the handler that runs the checks, in order, and builds the report.

A check passes, fails with a detail and a fix, or skips with a reason when it
does not apply: no bucket configured, an optional tool absent. A skip leaves
`healthy` true and sets no exit code. An optional subsystem cannot fail `doctor`
for a caller who does not use it, and nothing claims to be verified when it was
not. A failure keeps the envelope `ok` (the report ran) and sets the process
status to that check's exit code. A specific code wins over the generic 1,
whatever the order of the checks.

### skill

`AgentCli::skill()` registers a `skill` command that prints the CLI as an agent
skill: a `SKILL.md` with YAML frontmatter (`name`, `description`), the envelope
contract, every usage template (subcommands indented, raw and streaming
commands marked), the agent flags, and the exit-code and error-code
dictionaries. It is rendered from the live tree when the command runs, so
`.skill()` can sit anywhere in the builder chain, and the file cannot drift from
the code.

```bash
calc skill                            # the markdown, in result.markdown
calc skill --install=.claude/skills   # writes .claude/skills/calc/SKILL.md
```

Without `--install` the result holds `markdown` and `skill_name`.
`--install=<dir>` writes `<dir>/<name>/SKILL.md` and answers with the absolute
`path`, its `bytes` and `skill_name`; under `--dry-run` it writes nothing,
reports the path and adds `"dry_run": true`. `skill` reads `--dry-run` itself,
so the preview works with the agent flags off too. A shell does not expand `~`
after `=`, so `--install=~/skills` is refused with `INVALID_FLAG`, and a failed
write is `WRITE_FAILED`. `cli.skill_markdown()` returns the same document, for
a test to pin:

```rust
use agcli::AgentCli;

let cli = AgentCli::new("calc", "Agent-native calculator").version("1.0.0").skill();
let markdown = cli.skill_markdown();
assert!(markdown.starts_with("---\nname: calc\n"));
assert!(markdown.contains("`calc skill [--install=<dir>]`"));
assert!(cli.audit().is_clean());
```

The CLI's name is both the frontmatter `name` and the directory `--install`
writes, so `skill()` panics unless it is a slug: lowercase letters and digits,
with single hyphens between them. A mistake in the CLI definition fails the
build of the CLI, not an agent's invocation.

### The clock

Every timestamp comes from the run's clock, read when the thing happens: an
event's `ts` as the event is written, the envelope's `timestamp` when it is
built at the end of the run, and a `HANDLER_PANIC` envelope's at the panic.

The default clock reads `SOURCE_DATE_EPOCH` when it is set (the
reproducible-builds convention), else the system clock. A `SOURCE_DATE_EPOCH`
that is not plain ASCII digits naming a second up to 253402300799
(9999-12-31T23:59:59Z) fails the run with `INVALID_ENV`, exit code 2, before
anything runs: falling back to the wall clock would defeat the pin. A system
clock set before 1970 reads as 1970-01-01T00:00:00Z.

`AgentCli::clock(|| seconds)` replaces the default clock. A test pins it to get
byte-stable output, and a handler that stamps its own fields reads the same
clock with `req.now()`, so a `created_at` is pinned with the rest. A clock that
reads past `Timestamp::MAX` (9999-12-31T23:59:59Z) panics: no RFC 3339
timestamp can name that second, and writing a wrong one would be a silent
fallback.

### Panics

Under `run_env`, a handler that panics still ends the process with one
`HANDLER_PANIC` error envelope and exit code 1. `run_env` installs a panic hook
once per process. The hook calls the one installed before it, so the usual
panic message still reaches stderr. It acts only while a handler runs on the
panicking thread: in the handler's async body, in the synchronous code before
it, and in doctor checks. It writes the envelope to stdout, as the terminal
`"type":"error"` line for a streaming command, and to stderr for a raw command,
whose stdout is its own. Then it exits the process, so `run_env` does not
return.

The hook runs before any unwinding, so this works under `panic = "abort"` too.
A panic outside a handler, or in a task the handler spawned that the runtime
catches, behaves as it would without agcli. A stack overflow or an allocation
failure aborts before any hook runs.

`run` installs nothing. A panicking handler panics the caller, which is what a
test wants to see.

### Testing a CLI

```rust
use agcli::{AgentCli, Command, CommandOutput};
use serde_json::json;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let cli = AgentCli::new("app", "Example CLI").clock(|| 1_740_000_000).command(
    Command::new("status", "Show system health")
        .usage("app status")
        .handler(|_req| async move { Ok(CommandOutput::new(json!({ "healthy": true }))) }),
);

let run = cli.run(["app", "status"]).await;
assert_eq!(
    run.to_json(),
    r#"{"ok":true,"command":"app status","timestamp":"2025-02-19T21:20:00Z","exit_code":0,"result":{"healthy":true},"next_actions":[{"command":"app status","description":"Run this command template"},{"command":"app","description":"Inspect the full command tree"}]}"#
);
assert!(cli.audit().is_clean());
# }
```

`AgentCli::run(args)` takes argv with the program name first. It prints
nothing, touches no global state, and returns the `Execution`. Read the typed
envelope with `envelope()`, the line `print` would write with `to_json()`, and
a stream's events with `stream_events()`. A raw handler still writes to the
real stdout. With the clock pinned, the output is byte-stable.

`audit()` checks the tree without running it. Every declared `next_action` must
resolve to a real command, no command may be a dead end, and usage strings must
be well formed. It also warns about missing descriptions and usage strings. It
sees what is declared (descriptions, usage strings, `default_next_action`s),
not the `next_actions` a handler builds at run time.

Behaviour that needs a real process, such as the panic hook, live stdout or
`SOURCE_DATE_EPOCH`, needs a subprocess. agcli's own tests re-execute the test
binary for it.

### A checklist for a new command

- A description, and a usage string that starts with the program name.
- `next_actions` that follow from what just happened, with `params` for what
  varies and `value` for what the agent already knows.
- Errors with a code, a fix and an exit code, and `retryable` when a retry can
  work.
- Output that can grow is bounded, with `list_truncated` or
  `truncate_lines_with_file`.
- `--dry-run` honored and declared with `handles_dry_run()`, or not offered.
- Nothing on stdout but the envelope. Narrate on stderr, and use no colors and
  no terminal (TTY) checks: the output has to work when piped.
- `cli.audit().is_clean()` in a test.

## Wire contract

This is what a CLI built on agcli prints.
[`tests/golden_envelopes.rs`](https://github.com/matthiasdebernardini/agcli/blob/master/tests/golden_envelopes.rs)
pins the exact bytes.

### Envelopes

An answer is one JSON object on one line of stdout. Its keys come in this
order:

| Key | Present | Value |
|-----|---------|-------|
| `ok` | always | `true` on success, `false` on failure |
| `command` | always | the invocation as one shell line that reruns as printed: the program's file name, then every argument, single-quoted unless it is made only of ASCII letters, digits and `-_=/.:,@+` |
| `timestamp` | always | RFC 3339 (the internet date-time format) in UTC, to the second, from 1970 to 9999: `2026-06-10T14:42:17Z` |
| `exit_code` | always | the process status, 0 to 255 |
| `schema_version` | when the CLI sets `AgentCli::schema_version` | a string |
| `result` | on success | the command's payload |
| `select_warning` | on success, when `--select` could not do what it was asked | what went wrong, and the fields that exist |
| `error` | on failure | `message`, `code` and `retryable`, in that order |
| `fix` | on failure | what to do about it |
| `data` | on failure, when the error carries facts | any JSON |
| `next_actions` | always | an array, empty on success under `--quiet` |

A success with a non-zero `exit_code` is a report that ran and found a problem,
such as `doctor` on an unhealthy system. A failure's `exit_code` is 1 unless the
error sets one. Usage errors the framework raises are 2.

In Rust, an envelope's `timestamp` field is an `agcli::Timestamp`. It displays
and serializes as the RFC 3339 string, `epoch_secs()` gives the Unix seconds,
and it parses back from exactly the shape it prints.

### `next_actions`

Each action has `command`, `description` and, for a template, `params`, whose
entries hold any of `description`, `value`, `default`, `enum` and `required`,
in that order. How to read a template is under
[`next_actions`](#next_actions) above.

### NDJSON streams

A streaming command prints event lines, then exactly one terminal line.

- An event line has `type` first (`start`, `step`, `progress`, `log` or
  `event`), then its own keys, then `ts`. It never has `ok`.
- The terminal line is the envelope above with `type` (`result` or `error`) in
  front. It is always last, and there is always exactly one.
- Answers raised before argv parses (`PARSE_ERROR`, `INVALID_ENV`, a
  non-UTF-8 argument) cannot know the command streams. They are plain
  envelopes, without `type`, and decode as `Envelope`. So are the framework's
  own answers: the tree, help and the version.

The rule for a consumer: the line with `ok` is the answer, and it is the last
line.

### Raw commands

A raw command prints whatever its handler prints and exits with the handler's
code; nothing else reaches its stdout (see [Raw commands](#raw-commands)).

### Exit codes

| `ExitCode` | Code | Meaning |
|------------|------|---------|
| `SUCCESS` | 0 | success |
| `ERROR` | 1 | error (unclassified failure) |
| `USAGE` | 2 | usage (unknown command/subcommand/flag, malformed or repeated flag, bad or missing argument or flag value, unsupported `--dry-run`, invalid `SOURCE_DATE_EPOCH`) |
| `NOT_FOUND` | 3 | `not_found` (a requested resource does not exist) |
| `AUTH` | 4 | auth (authentication or authorization failure) |
| `API` | 5 | api (an upstream call failed) |
| `RATE_LIMITED` | 7 | `rate_limited` (back off and retry) |

`ExitCode::new(code)` builds any other status, such as the 1 a raw `grep`
returns for "no matches". The root tree publishes this table as `exit_codes`,
keyed by the number.

### Error codes

The codes the framework raises, read from `error.code`. A command adds codes of
its own. Every error, whatever its code, also carries `retryable` and `fix`.

| `error.code` | Exit code | Meaning |
|--------------|-----------|---------|
| `PARSE_ERROR` | 2 | the command line could not be parsed (a malformed or repeated flag, or an argument that is not UTF-8); fix says how to repair it |
| `UNKNOWN_COMMAND` | 2 | no such command; fix lists valid names |
| `UNKNOWN_SUBCOMMAND` | 2 | no such subcommand; fix lists valid names |
| `UNKNOWN_FLAG` | 2 | flag not declared by this command; fix lists valid flags |
| `EXTRA_ARG` | 2 | more positional arguments than the usage template declares |
| `MISSING_ARG` | 2 | a required positional argument is absent |
| `INVALID_ARG` | 2 | a positional argument failed to parse |
| `INVALID_FLAG` | 2 | a flag value failed to parse; a boolean flag takes no value or one of true/false, 1/0, yes/no, on/off |
| `INVALID_ENV` | 2 | an environment variable the framework reads (`SOURCE_DATE_EPOCH`) is invalid |
| `MISSING_HANDLER` | 2 | the command has neither a handler nor subcommands, so nothing can run |
| `DRY_RUN_UNSUPPORTED` | 2 | `--dry-run` passed to a command without a preview mode; nothing was changed |
| `HANDLER_PANIC` | 1 | a bug in the command handler, not the invocation |
| `STREAM_WRITE_FAILED` | 1 | a streaming command could not write its event stream |
| `SERIALIZATION_FAILED` | 1 | the handler's result could not be serialized |
| `WRITE_FAILED` | 1 | the framework could not write a file the command was asked to produce |

The root tree publishes this table as `error_codes`.

### Decoding

With the `deserialize` feature, the wire types implement `Deserialize`:
`Envelope`, `SuccessEnvelope`, `ErrorEnvelope`, `StreamLine`, `StreamEvent`,
`NextAction`, `ActionParam`, `Timestamp`, `ExitCode`, `ErrorCode`,
`TruncatedEntries` and `AuditReport`, among others. Decoding is strict about
what it knows: a missing or malformed known key is an error, and so is a
timestamp that names no real date, a `null` where the framework would leave the
key out, an empty `error.code`, an error envelope with `exit_code` 0, or a
`TruncatedEntries` whose counts disagree. A key it does not know is ignored, so
a reader keeps working on output from a later agcli that only added a field.

```rust
# #[cfg(feature = "deserialize")]
# {
use agcli::{Envelope, StreamEvent, StreamLine, Timestamp};

// Every line of a stream decodes as a `StreamLine`: an event, or the terminal envelope.
let line: StreamLine = serde_json::from_str(
    r#"{"type":"log","level":"warn","message":"cache cold","ts":"2026-06-10T14:42:17Z"}"#,
)
.expect("an event line");
let StreamLine::Event { event, timestamp } = line else { panic!("an event line") };
assert!(matches!(event, StreamEvent::Log { .. }));
assert_eq!(timestamp, "2026-06-10T14:42:17Z".parse::<Timestamp>().expect("RFC 3339"));

// An answer raised before argv parsed has no `type`: decode it as an `Envelope`.
let answer: Envelope = serde_json::from_str(
    r#"{"ok":false,"command":"app --=x","timestamp":"2026-06-10T14:42:17Z","exit_code":2,"error":{"message":"invalid flag syntax: --=x","code":"PARSE_ERROR","retryable":false},"fix":"Spell a long flag `--name` or `--name=value`, and a short flag `-n` or `-n=value`. Bundled short flags (`-abc`) take no value, so give a valued short flag on its own (`-a -b -c=5`).","next_actions":[{"command":"app","description":"Inspect the full command tree"}]}"#,
)
.expect("an envelope");
assert!(!answer.ok());
# }
```

## Platforms and dependencies

- agcli supports macOS and Linux.
- It needs Rust 1.88 or newer.
- Its only dependencies are `serde` and `serde_json`. It needs no async
  runtime: dispatch is async, and any executor can drive it.
- `benches/core.rs` measures root invocation, command execution, argv parsing,
  truncation and streaming with criterion: `cargo bench`.

## Examples

- [`examples/calc.rs`](https://github.com/matthiasdebernardini/agcli/blob/master/examples/calc.rs):
  the calculator above.
- [`examples/ops.rs`](https://github.com/matthiasdebernardini/agcli/blob/master/examples/ops.rs):
  `status`, `logs` with truncation and typed flag errors, a streaming
  `deploy`, and a raw `echo`.

```bash
cargo run --example ops -- deploy web
cargo run --example ops -- logs worker --lines=5
cargo run --example ops -- echo -C 3 --not-a-flag
```

## License

MIT.
