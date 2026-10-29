# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Breaking changes

On the wire, for agents and the programs that read agcli output:

- **A secret in argv is no longer echoed.** The `command` field, and the error
    text that quotes an argument, mask the value of any flag the command's
    usage does not declare, in `--flag=value`, `--flag value`, `-x=value` and
    `-x value`: `app run --passphrase=hunter2` reports
    `app run --passphrase=<redacted>`. The `PARSE_ERROR` for a malformed token
    such as `-abc=5` quotes it as `-abc=<redacted>`, a line that fails to
    parse masks every token that is not a flag name or a command name, and the
    flag values in a raw command's tokens are masked. A line that used to
    rerun as printed no longer does when it carried such a value: declare the
    flag in the usage (`--passphrase=<value>`) to keep its value in `command`.

    In Rust, `Invocation::command_line` returns the masked line inside a
    handler, as the envelope shows it; `Invocation::parse` still returns it
    verbatim.

- **A command's name is never a flag's value.** A bare flag (no `=`) takes
    the next token as its value only when that token does not name a
    command where a command name can go; otherwise the flag is given no
    value and the token is the command. A flag misplaced before a command's
    name now gets the error that names it: `app --max grep pat` is
    `INVALID_FLAG` "flag --max does not reach raw command `grep`",
    `app --bogus grep pat` and `calc --bogus add 1 2` are `UNKNOWN_FLAG`, and
    `ops --lines logs app` runs `logs`, which answers that `--lines` was given
    no value. All of these used to be `UNKNOWN_COMMAND` for the next word.
    Write `--flag=value` to pass a value spelled like a command.

    `app --name status status`, which set `--name` to `status`, becomes
    `app --name=status status`. Under `without_reserved_flags()`,
    `app --profile grep grep pat` now hands raw `grep` `["grep", "pat"]`,
    not `["pat"]`.

### Added

- **`Command::secret(name)`** masks a declared flag or positional as
    `<redacted>` in the `command` field and every error's text. The handler
    still reads the real value. `name` is how the usage spells it, without
    dashes or angle brackets.
- **`AgentCli::redact_arg(hook)`** takes `Fn(&str) -> Option<String>`, applied
    to each argv element before an envelope echoes it, for patterns only the
    app knows, such as `field=value` assignments. See "Secrets in argv" in the
    README.
- **`audit()` reports `UNDECLARED_SECRET`**, a `Command::secret` name the usage
    does not declare, and **`SECRET_ON_RAW_COMMAND`**, a secret marked on a raw
    command, whose tokens are never parsed. Both are errors, since neither masks
    anything.

## [0.18.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.18.0) - 2026-10-24

This release redesigns the Rust API. Handlers are plain async closures, agcli
no longer depends on tokio, the framework writes a streaming command's terminal
line, and exit and error codes are types. The wire contract is stricter: a
non-boolean value, a repeated flag or a malformed `SOURCE_DATE_EPOCH` is an
error, not a guess. Breaking changes are a **minor** bump pre-1.0.

### Breaking changes

In Rust:

- **Handlers are plain async closures over an owned `CommandRequest`.** The
    request has no lifetime and is `Clone` and `Debug`, so a handler moves it
    into its future, and the framework boxes the future.

    `.handler(|req, _ctx| { let a = req.arg_parse::<f64>(0, "a"); Box::pin(async move { .. }) })`
    becomes `.handler(|req| async move { let a: f64 = req.arg_parse(0, "a")?; .. })`.
- **`ExecutionContext` is gone**, with `run_env_with_context` and
    `run_argv_with_context`. Share state the way any closure does, by
    capturing it.

    `ctx.set("seen", value)` becomes a write through a captured
    `Arc<Mutex<_>>`.
- **`run_argv` is `run`.**

    `cli.run_argv(["app", "status"]).await` becomes
    `cli.run(["app", "status"]).await`.
- **Under `run_env`, a handler panic ends the process from a panic hook.**
    `catch_unwind` is gone. The hook writes the `HANDLER_PANIC` envelope and
    exits with code 1, so `run_env` does not return. The Fixed section lists
    what the hook covers.

    Code after `cli.run_env().await` no longer runs when a handler panics.
- **Raw handlers take the argv tail by value and resolve to an `ExitCode`.**

    `.raw_handler(|args, _ctx| { let args = args.to_vec(); Box::pin(async move { ..; 0 }) })`
    becomes `.raw_handler(|args| async move { ..; ExitCode::SUCCESS })`.
- **A command has one handler slot.** `handler`, `stream_handler` and
    `raw_handler` each fill it, and the last call wins. A raw handler used to
    win over a normal one whatever the order, and `audit()` reported
    `RAW_COMMAND_HAS_HANDLER`; that finding is gone. `doctor_with` replaces
    whatever handler its command had.

    `.raw_handler(raw).handler(normal)` runs `normal`.
- **Streaming moves into the framework.** `Command::stream_handler` hands the
    handler a `StreamWriter` for its events, and the framework writes the one
    terminal line from the handler's return value. `NdjsonEmitter`,
    `FlushPolicy`, `StreamEmitError`, the terminal `StreamEvent::Result` and
    `StreamEvent::Error` variants, `StreamEvent::result_from_envelope` and
    `StreamEvent::error_from_envelope` are gone. `StreamEvent` variants lose
    their `ts`: the writer stamps each event from the run's clock and flushes
    every line. `StreamWriter::emit` takes `&self`, and the writer is `Clone`,
    so a task the handler spawns can emit through a clone; a clone kept past
    the handler's return is refused with `StreamWriteError::Closed`.

    `emitter.emit(StreamEvent::Step { .., ts }).await?; emitter.emit_result(envelope).await?`
    becomes `out.emit(&StreamEvent::Step { .. })?; Ok(CommandOutput::new(..))`.
- **`ActionParam::default` is `default_value`.** The builder shadowed the
    derived `Default`, so `ActionParam::default()` did not compile. The wire
    key stays `default`.

    `ActionParam::new().default("worker")` becomes
    `ActionParam::new().default_value("worker")`.
- **tokio is no longer a dependency.** Dispatch stays async, and any executor
    can drive `run_env` and `run`. `truncate_lines_with_file` and
    `TruncatedEntries::cleanup` are synchronous, and `read_stdin` is gone.

    `truncate_lines_with_file(lines, 20, "logs").await?` becomes
    `truncate_lines_with_file(lines, 20, "logs")?`, and
    `agcli::read_stdin().await?` becomes
    `std::io::read_to_string(std::io::stdin())?`.
- **`truncate_lines_with_file` refuses a `max_lines` of 0 and an invalid
    `file_prefix`** with `io::ErrorKind::InvalidInput`, before writing
    anything. It used to treat 0 as 1 and replace an invalid prefix with
    `agcli-output`. A valid prefix is non-empty ASCII letters, digits, `-` and
    `_`.

    `truncate_lines_with_file(lines, 0, "logs")` returns an error.
- **Doctor checks have one constructor, and it takes the request.**
    `Check::with_request` is gone.

    `Check::new("ping", || Box::pin(async { CheckResult::pass() }))` becomes
    `Check::new("ping", |_req| async { CheckResult::pass() })`.
- **`ExitCode` is a `u8` newtype**, not a namespace of `i32` constants.
    `CommandError::exit_code`, `CommandOutput::exit_code`, `Check::exit_code`,
    raw handlers, `Execution::exit_code`, `Execution::print` and
    `Envelope::exit_code` take or return it. `ExitCode::new` builds any status
    and `get` reads it. `TryFrom<i32>` fails outside `0..=255`, so nothing is
    masked any more. `From<ExitCode> for std::process::ExitCode` lets `main`
    return one.

    `assert_eq!(run.exit_code(), 0)` becomes
    `assert_eq!(run.exit_code(), ExitCode::SUCCESS)`.
- **Error codes are `ErrorCode` values, and `CommandError::new` takes the code
    first**, so the code, the message and the fix cannot trade places. The
    framework's codes are constants such as `ErrorCode::INVALID_ARG`; a
    command names its own with `ErrorCode::new`, or `ErrorCode::owned` for a
    `String` built at run time (`format!("HTTP_{status}")`). A code is never
    empty: both constructors panic on one. `CommandError::code` and
    `ErrorBody::code` are `ErrorCode`.

    `CommandError::new("no such issue", "NOT_FOUND", "Check the id")` becomes
    `CommandError::new(ErrorCode::new("NOT_FOUND"), "no such issue", "Check the id")`.
- **The framework builds every envelope.** `SuccessEnvelope::new`,
    `ErrorEnvelope::new`, `ErrorBody::new`, and the envelopes'
    `schema_version`, `exit_code`, `retryable` and `data` builders are
    private. `SuccessEnvelope`, `ErrorEnvelope`, `ErrorBody` and
    `CommandError` are `#[non_exhaustive]`, so `CommandError::new`, which
    requires a fix, is the only way to build an error. Read envelopes from
    `Execution::envelope`, or decode them with the `deserialize` feature.

    `ErrorEnvelope::new(command, message, code, fix, actions)` becomes
    `Err(CommandError::new(code, message, fix))` from a handler.
- **Envelope timestamps are `Timestamp` values.** `SuccessEnvelope::timestamp`
    and `ErrorEnvelope::timestamp` are an `agcli::Timestamp`: a second from
    1970-01-01T00:00:00Z to 9999-12-31T23:59:59Z that displays and serializes
    as the RFC 3339 string and parses back strictly. `StreamLine::Event`
    carries `timestamp: Timestamp`; its wire key stays `ts`.
    `SuccessEnvelope` gains `select_warning`.

    `envelope.timestamp` (a `u64`) becomes `envelope.timestamp.epoch_secs()`.
- **`Invocation::parse(args, is_bool)` replaces `parse_invocation` and
    `parse_invocation_with_bool_flags`.** `Invocation::into_command_line` is
    gone, `positional_indices` is private, and `ParseInvocationError` gains
    `RepeatedFlag`.

    `parse_invocation_with_bool_flags(args, |flag| flag == "force")` becomes
    `Invocation::parse(args, |flag| flag == "force")`.
- **`AgentCli::reserved_flags(false)` is `without_reserved_flags()`.** `true`
    was the default and meant nothing.

    `.reserved_flags(false)` becomes `.without_reserved_flags()`.
- **`CommandRequest::prompt` is gone, and `CommandRequest::select` returns an
    iterator** over the requested fields, empty when `--select` is absent.

    `req.select().unwrap_or_default()` becomes
    `req.select().collect::<Vec<_>>()`.
- **`CommandOutput::from_serializable` is `try_from_serializable`**, so the
    name says it can fail.

    `CommandOutput::from_serializable(&value)` becomes
    `CommandOutput::try_from_serializable(&value)`.
- **`CheckResult::failed` and `CheckResult::skipped` are `is_failed` and
    `is_skipped`**, named like `is_ok`.

    `result.failed()` becomes `result.is_failed()`.
- **`reserved_flag_names()` no longer lists `version`**, and lists the flags
    in the order the root tree documents them. `--help`, `-h`, `--version`
    and `-V` are reserved always, apart from the agent flags.

    `reserved_flag_names().contains(&"version")` is now `false`.
- **More public types are `#[non_exhaustive]`**: the structs `NextAction`,
    `ActionParam`, `TruncatedEntries`, `CheckResult`, `AuditFinding` and
    `AuditReport`, and the enums `ParseInvocationError` and
    `StreamWriteError`. Get the structs from `NextAction::new`,
    `ActionParam::new` and the other functions that return them, and give a
    `match` on the enums a wildcard arm.

    `CheckResult { status: CheckStatus::Pass, detail: None, fix: None }`
    becomes `CheckResult::pass()`.
- **Builder mistakes panic in every build.** A duplicate name in
    `AgentCli::command` or `Command::subcommand`, and
    `CommandError::exit_code(ExitCode::SUCCESS)`, were `debug_assert!`s: a
    release build silently replaced the first command, or printed an error
    envelope with exit code 0. Two more mistakes panic the same way:
    `CommandOutput::list_truncated(items, total)` with a `total` below the
    item count, which reported `count: 3, total: 1, truncated: false`, and
    `AgentCli::skill()` on a CLI name that is not a slug, whose frontmatter
    `name` and `--install` directory no harness accepts. The
    `SKILL_NAME_INVALID` audit finding is gone with it: such a CLI cannot be
    built. Under `run_env`, an assert inside a handler becomes a
    `HANDLER_PANIC` envelope.

    `.command(status).command(status_again)` panics in release builds too.
- **The `deserialize` feature is strict about the keys it knows.** A missing
    or malformed known key is an error: no epoch-integer `timestamp`, no
    unparseable timestamp read as 0, no timestamp naming an impossible date
    such as 2026-02-29, and no default for a missing `exit_code`, `dropped`
    or event field. Shapes the framework never writes are refused too: a
    `null` for `schema_version`, `select_warning` or `data` (the key is left
    out instead), an empty `error.code`, an error envelope with `exit_code`
    0, and a `TruncatedEntries` whose counts disagree (`lines` must count
    `entries`, `dropped` must be `total - lines`, `truncated` must be true
    exactly when lines were dropped, and `full_output` must be present
    exactly when truncated). Unknown keys are still ignored, so a reader
    keeps working on output from a later agcli that only added a field.
    `StreamEvent` decodes event lines only; `StreamLine` decodes any line of
    a stream.

    `serde_json::from_str::<StreamEvent>(terminal_line)` becomes
    `serde_json::from_str::<StreamLine>(terminal_line)`.

On the wire, for agents and the programs that read agcli output:

- **`select_warning` is an envelope key**, between `result` and
    `next_actions`. It used to be inserted into the handler's `result`
    object, where it could replace the handler's own `select_warning`, and an
    array result had nowhere to put it.

    `result.select_warning` becomes the top-level `select_warning`.
- **The `command` field reruns as printed**: the program's file name, then
    every argument, single-quoted (POSIX style) unless it is made only of
    shell-safe characters. Every envelope renders it this way, including a
    `PARSE_ERROR` raised before argv parses, which used to carry the full
    argv[0] path, and the raw command, `--select` advertisement and panic
    envelopes.

    `"command": "app note two words"` becomes
    `"command": "app note 'two words'"`.
- **Doctor check entries drop `ok`.** `status` (`pass`, `fail` or `skip`) is
    the one outcome field.

    `checks[i].ok` becomes `checks[i].status != "fail"`.
- **`--version` with no version set omits the `version` key** instead of
    reporting `"unknown"`.

    `{"name":"app","version":"unknown"}` becomes `{"name":"app"}`.
- **A malformed `SOURCE_DATE_EPOCH` is an `INVALID_ENV` error**, exit code 2,
    checked before anything runs. The reproducible-builds spec requires an
    error; agcli used to clamp a malformed, negative or too-large value.

    `SOURCE_DATE_EPOCH=-5 app status` answered at `1970-01-01T00:00:00Z`; it
    now answers `INVALID_ENV`.
- **Boolean flag values are strict.** Only `true`/`false`, `1`/`0`,
    `yes`/`no` and `on`/`off` count, in any ASCII case and untrimmed, and a
    bare flag is `true`. Any other value of a reserved or framework boolean is
    `INVALID_FLAG`, exit code 2, with the named command's templates as
    `next_actions`, and as a terminal line for a streaming command. Any value
    but an off spelling used to turn the flag on.

    `--quiet=maybe` turned quiet on; it now fails with `INVALID_FLAG`.
- **A flag given twice is a `PARSE_ERROR`** naming it (`--select given
    twice`), in any spelling: `--n=1 -n 2`, `-vv`. The last value used to win
    silently.

    `--select=a --select=b` projected `b`; it now fails.
- **Bundled short flags with a value are a `PARSE_ERROR`.**

    `-abc=5` set the flags `a`, `b`, `c`, `=` and `5`; it now fails, naming the
    token.
- **`--help`, `-h`, `--version` and `-V` always belong to the framework**,
    parsed as booleans even under `without_reserved_flags()`. `-V status`
    keeps `status` as a positional, which then has to name a command, and
    `--version=false` is off, so the root tree answers. A raw command still
    owns them after its name.

    `app -V foo` answered the version; it now keeps `foo` as a positional
    and reports `UNKNOWN_COMMAND` for it.
- **`--version` and `-V` answer wherever they appear**, as `--help` does,
    once the positionals name a command or nothing, and help wins when both
    are given. Flags are checked against the named command's usage. The
    `version` alias takes no argument.

    `calc add 1 2 --version` ran `add`; it now answers the version.
    `calc version extra` was `UNKNOWN_COMMAND` for `version`; it is now
    `EXTRA_ARG` for `extra`.
- **The root, help and version answers reject unknown flags** with
    `UNKNOWN_FLAG`, exit code 2. Help for a command checks flags against that
    command's usage, and a group without a usage string declares no flags.

    `calc --bogus` printed the tree; it now fails. `app db --bogus` printed
    the group's tree with the flag dropped; it now fails.
- **The tokens before a raw command's name are the framework's.** A help or
    version flag there is answered, a reserved or framework boolean given a
    value that is not a boolean is `INVALID_FLAG`, and a malformed token there
    is `PARSE_ERROR`. Everything after the name stays the command's, as before.

    `app --help grep` ran grep with no arguments, and `app -h grep pat` ran it
    with `pat`; both now answer help for `grep`. `app --quiet=maybe grep pat`
    ran grep; it is now `INVALID_FLAG`.
- **A `-` followed by a digit is a positional.** `-0x10` and `-1,000` were
    read as bundles of short flags.

    `app add 1 -0x10` failed with `PARSE_ERROR: -0 given twice`; it now reaches
    the handler, whose `arg_parse` reports `INVALID_ARG` for `-0x10`.
- **An explicit `--select` wins over `compact_fields`.** Beside `--select`,
    `--compact` only drops nulls and empty containers from the selection.

    `app get --select=body --compact` on a command declaring
    `compact_fields(["id", "name"])` answered `"result": {}`; it now answers
    the selected `body`.
- **A list's `fields` leaves out keys `--select` cannot spell**: one holding
    `.` or `,`, or padded with whitespace.

    `["items.a.b"]` was advertised for a row key `a.b` and projected to a
    miss; the key is now left out of `fields`.

### Added

- **`Command::stream_handler` and `StreamWriter`.** The handler writes
    `start`, `step`, `progress`, `log` and `event` lines with
    `StreamWriter::emit`. When it returns, the framework writes exactly one
    terminal line: the envelope with `"type":"result"` or `"type":"error"` in
    front, to which `--select`, `--quiet` and `next_actions` apply. A usage
    error on a streaming command is a terminal line too. Under `run_env` the
    events reach stdout as they are written; under `run` they are captured,
    and `Execution::stream_events` returns them. A failed write is a
    `StreamWriteError` that poisons the writer, and `?` turns it into a
    `STREAM_WRITE_FAILED` error. `Command::is_stream` and
    `Execution::is_stream` tell a streaming command apart; the root tree and
    help mark it `"stream": true`, and the generated skill says it streams.
    `examples/ops.rs` gains a streaming `deploy`.
- **`StreamLine`**, with the `deserialize` feature, decodes any line of a
    stream: an event, or the terminal `result` or `error` envelope.
- **`AgentCli::clock`** sets the clock in Unix seconds. The run reads it
    whenever it writes a timestamp: an event's `ts` as the event is written,
    the envelope's `timestamp` when it is built at the end of the run, and a
    `HANDLER_PANIC` envelope's at the panic. Tests pin it instead of setting
    `SOURCE_DATE_EPOCH`. A reading past `Timestamp::MAX`
    (9999-12-31T23:59:59Z) panics rather than write a wrong timestamp.
- **`CommandRequest::now`** reads the run's clock, so a handler that stamps
    its own fields (`created_at`) is byte-stable under a pinned clock too.
- **`Envelope::timestamp`** is public, like `command`, `ok` and `exit_code`.
- **`Timestamp` and `ParseTimestampError`.** `Timestamp::UNIX_EPOCH`,
    `Timestamp::MAX`, `from_epoch_secs` (`None` past `MAX`), `epoch_secs`,
    `Display` as RFC 3339 UTC, and a strict `FromStr` that accepts exactly
    `YYYY-MM-DDTHH:MM:SSZ` on a real calendar date.
- **`CommandOutput::next_actions` and `CommandError::next_actions` take any
    `IntoIterator<Item = NextAction>`**, not only a `Vec`.
- **`CommandRequest::flag_on`** reads a command's own boolean flag strictly:
    `false` when absent, the boolean a valid spelling names, else
    `INVALID_FLAG`. `req.flag("force").is_some()` read `--force=false` as on.
- **The `INVALID_ENV` and `STREAM_WRITE_FAILED` error codes**, published in
    `error_codes` and the generated skill.
- **The root tree and the generated skill describe the whole contract.**
    `agent_flags` says that `--quiet` keeps an error's `next_actions`, that a
    `--select` miss is reported in a top-level `select_warning`, and why
    `--json` is reserved. `exit_codes` and `error_codes` name malformed and
    repeated flags, the boolean spellings, and when `MISSING_HANDLER` happens.
    The skill's contract adds `select_warning`, that `--help` and `--version`
    are answered anywhere on the line, and that only a stream's terminal line
    carries `ok`.
- **`audit()` reports `INTERCEPTED_FLAG_DECLARED`** (error) for a usage
    string that declares `-h`, `--help`, `-V` or `--version`, which the
    framework always answers itself. Raw commands are exempt.
- **`audit()` reports `USAGE_PATH_MISMATCH`** (error) for a usage string
    whose command tokens, after the program name, do not name the command it
    documents: the tree and every gate error advertise `app fetch <id>` for
    `get`, a template that runs as `UNKNOWN_COMMAND`.
- **The generated skill says what a raw command prints**: its own output and
    exit code, with no envelope, when any command is raw.
- **More standard traits.** `Debug` for `Check` and `StreamWriter`; `Clone`
    for `StreamWriter`; `Clone` and `Debug` for `CommandRequest`; `Display`
    for `ExitCode` (the number); `Eq` for `NextAction`, `ActionParam`,
    `CommandOutput`, `CommandError`, `SuccessEnvelope`, `ErrorEnvelope`,
    `Envelope`, `Execution` and `StreamEvent`; `Ord` for `ErrorCode`; `Copy`,
    `Hash` and `Ord` for `LogLevel` (`Info` < `Warn` < `Error`); `Copy` and
    `Hash` for `StepStatus`; `Hash` and `Ord` for `AuditSeverity`
    (`Warning` < `Error`); `Hash` for `CheckStatus`, which is now exhaustive
    like the other wire enums, so a `match` notices a new outcome. Every
    public type is `Send + Sync`, and a test pins it.

### Fixed

- **A panicking handler always ends with one `HANDLER_PANIC` envelope under
    `run_env`.** The panic hook covers the synchronous code a handler runs
    before its future, which `catch_unwind` around the poll missed. It works
    under `panic = "abort"`, writes the envelope as the terminal line of a
    streaming command and to stderr for a raw one, stamps it with the clock
    at the panic, and calls the previously installed hook first. Panics
    outside handlers, and panics in tasks a handler spawned that the runtime
    catches, behave as they would without agcli.
- **A non-UTF-8 argument is a `PARSE_ERROR` envelope** naming its position
    (`argument 2 is not valid UTF-8`). `run_env` read argv with
    `std::env::args`, which panics on one.
- **Printing never panics.** `Execution::print` and `finish` write through
    locked handles: a closed stdout (`EPIPE`, a broken pipe) is ignored, and
    any other write failure is reported on stderr. `println!` panicked on a
    closed pipe.
- **A streaming command prints exactly one terminal line.** A handler that
    streamed through `NdjsonEmitter` still returned an envelope, so stdout got
    two terminal lines.
- **Deep command trees are documented and audited in full.** A depth limit of
    32 levels silently cut the root tree, help, the skill and `audit()` short.
- **`flag_parse` names a one-letter flag with one dash** (`flag -n is not
    valid`), like every other message, instead of `--n`.
- **The `UNKNOWN_FLAG` fix lists the reserved flags from the list the parser
    reads**, and says that `--help` and `--version` are accepted too.
- **The root tree's `commands` is built as JSON directly**, so it can no
    longer fall back to a string.
- **`examples/ops.rs` teaches typed errors.** `ops logs` refuses a missing
    `<source>` with `MISSING_ARG` and a `--lines` that is not a positive count
    with `INVALID_FLAG`, instead of defaulting to `worker` and 20.
- **A `PARSE_ERROR` says how to fix its cause.** A repeated `--select` says to
    pass every field in one comma-separated list, another repeated flag says
    to pass it once, and a malformed flag says how to spell one. Every
    `PARSE_ERROR` used to say "Use valid CLI syntax. Run the root command to
    inspect command templates."
- **A literal `next_action` runs as printed.** A usage string without a
    placeholder became a literal action that kept its optional parts, so
    `wokhei gateway stream [--follow]` passed `[--follow]` as an argument and
    failed with `EXTRA_ARG`. The literal now drops them:
    `wokhei gateway stream`. A variadic placeholder is a param like any
    other: `t run <cmd...>` created none, so the action was literal and
    printed `<cmd...>`; it is now a template with the required param
    `cmd...`.
- **`--select` never wipes a result, on any shape.** A dot path that reached
    nothing left an empty container behind (`--select=a.zzz` gave
    `{"a":{}}`, `--select=items.zzz` on a list gave `{"items":[{},{}]}`),
    so the result was gone with `ok: true` and no warning; it is a miss like
    any other, and the full result comes back with a `select_warning`. A
    select that matched no field in any row of an array returned rows of
    `{}` without a warning; it warns, naming the keys found across the rows.
    A partial miss on an array (`--select=id,naem`) was dropped in silence,
    as a dot-path miss beside a hit was; both warn as an object does. A bare
    `--select` reached the projection as the parser's `"true"`, so the
    warning blamed a `--select=true` nobody typed; it now says the flag was
    given no field names. A result that is not an object or an array of
    objects was projected to itself in silence; it now carries a warning.
- **The `--select` advertisement runs as printed.** It appended
    `--select=<fields>` after a `--`, where the filled template parsed it as
    a positional and grew on every hop; the flag now sits before the first
    `--`.
- **`skill --install` honors `--dry-run` with the agent flags off.** It read
    the flag only through the reserved set, so under `without_reserved_flags()`
    it wrote the file. The skill reads it itself, and refuses a value that is
    not a boolean with `INVALID_FLAG`.
- **The `help` alias yields to a CLI's own `help` command**, as the
    `version` alias already did. The CLI's `help` command was unreachable.
- **Wire prose is plain ASCII English.** The `--select` advertisement, the
    non-finite `INVALID_ARG` and `INVALID_FLAG` fixes, the `EXTRA_ARG` range
    (`1 to 2`, not `1–2`), the unbalanced-brackets audit message and the
    generated skill's list items (`` `item`: description ``) carry no dashes
    or multiplication signs. The list `guidance` no longer names a `--limit`
    the command may not have; it points at the command's own flags and
    `--select`. The lone root action of a CLI with no commands reads "Inspect
    the full command tree", like every other root action.
- **The pre-commit hooks and `just publish-check` build through `mbx`**, like
    the gate.
- **An unknown flag before a raw command's name is `UNKNOWN_FLAG`**, with
    the fix the root line gives, and the handler does not run. Under
    `without_reserved_flags()` nothing is refused. Flags after the name stay
    the command's.

    `app --bogus=1 grep pat` ran grep with `pat` and dropped `--bogus=1`; it
    is now `UNKNOWN_FLAG`.
- **A reserved agent flag before a raw command's name is refused, not
    dropped.** A raw handler receives only the tokens after its name, so the
    flag could reach nothing. `--dry-run` there is `DRY_RUN_UNSUPPORTED`, and
    every other reserved flag but `--json` is `INVALID_FLAG`; nothing runs.
    `--json`, `--help` and `--version` stay accepted, and under
    `without_reserved_flags()` nothing is refused.

    `app --dry-run grep pat` ran grep for real with `pat`; it is now
    `DRY_RUN_UNSUPPORTED`. `app --yes grep pat` ran grep without the `--yes`;
    it is now `INVALID_FLAG` "flag --yes does not reach raw command `grep`".
- **`--compact` beside `--select` no longer empties the selection in
    silence.** When every selected field is null or empty, the selection is
    returned uncompacted with a `select_warning`.

    `app get 1 --select=note --compact` on a null `note` answered
    `result: {}` with no warning; it now answers `result: {"note": null}` and
    a `select_warning` saying `--compact` removed every selected field.
- **`flag_parse` refuses a bare value flag as given no value.** A bare
    `--limit` reads as `"true"`, and the error blamed that value.

    `req.flag_parse::<usize>("limit")` on `app get 1 --limit` gave
    `flag --limit is not valid: "true"`; it now gives `INVALID_FLAG`
    `flag --limit was given no value`, fix `Pass a value: --limit=<value>`.

### Internal

- `src/cli.rs`, about 5,300 lines, is split into one module per primary item,
    with unit tests beside their code. Free functions are called through their
    module (`usage::flag_display`). Unreachable branches, among them a
    `MISSING_HANDLER` path in raw dispatch, and a second copy of the reserved
    flag names are gone. Test names and crate-private helpers follow the
    current API.
- The crate-wide `#![allow(clippy::pedantic)]` is gone. The crate is clean
    under clippy pedantic and the repo's ast-grep rules with no lint
    suppressions anywhere. Builders, constructors, getters, `CommandError` and
    `Execution` are `#[must_use]`, so dropping a builder's result or an
    `Execution` that was never printed warns. Functions that fail or panic
    document `# Errors` and `# Panics`.
- `ci.yml` runs the gate on every push to `master` and every pull request:
    rustfmt, cargo-machete, clippy pedantic with and without
    `--all-features`, nextest, doctests, ast-grep, a check for lint
    suppressions, and rustdoc with `-D warnings`. A second job checks the MSRV
    (minimum supported Rust version) named by `rust-version`.
- `release.yml` runs that gate before it publishes, fails when CHANGELOG.md
    has no `## [X.Y.Z]` heading for the tag, and takes the GitHub Release
    notes from that section. The skip-if-already-published branch and
    git-cliff are gone, with `cliff.toml`. Every action is on its latest
    release, pinned by commit SHA.
- `rust-version = "1.88"`: 1.87 fails to build the crate.
- Packaging: an `include` list (`src`, `examples`, `tests`, `benches`,
    `Cargo.toml`, `README.md`, `CHANGELOG.md`, `LICENSE`) replaces `exclude`,
    docs.rs builds with every feature, so the `deserialize` items are
    documented, and the `homepage` that repeated `repository` is gone.
- `just gate` runs the CI gate through `mbx`. The `release` and `changelog`
    recipes are gone: they bypassed the tag-driven flow and used the BSD-only
    `sed -i ''`. The pre-commit hooks run clippy with `--all-features` and
    call `ast-grep scan`.
- criterion 0.8, and a refreshed `Cargo.lock`.
- Tests: the `tests/kill_*.rs` mutation-triage files are folded into
    `argv_parsing.rs`, `suggestions.rs`, `usage_templates.rs` and
    `golden_envelopes.rs`. Tests that need a process re-execute the test
    binary instead of calling `cargo run`, which takes the raw end-to-end
    test from about 11 s to 0.04 s; every re-executed child writes a
    sentinel line to stderr, and the parent asserts it once, with the
    harness's `running 1 test` line, so a child the filter never reached, or
    a filter that matched a second test, fails loudly. No test sets an
    environment variable; they pin the clock. New suites: `panic_hook.rs`,
    `streaming.rs`, `clock.rs`, `ops_example.rs` and `calc_example.rs`, plus
    a serialize-then-deserialize test for every public wire type. The example
    tests crawl every literal `next_action` an example prints and run it.
- `examples/calc.rs` builds its CLI in `fn cli()`, which the README's
    calculator copies and `tests/calc_example.rs` drives in-process.
- Docs: README.md is the user guide and the crate documentation, and every
    Rust block in it is a doctest. Every public item has rustdoc, and
    `#![warn(missing_docs)]` keeps it that way. No prose in the crate, the
    tests or the examples carries an em-dash. AGENTS.md is the one guide to
    working on the repo, and CLAUDE.md imports it. `design.md`, a copy of an
    upstream skill, is gone; its agcli-specific content is in the README.
    `agent_ergonomics_audit/` is gone; nothing read it.

## [0.17.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.17.0) - 2026-08-21

Help becomes a skill, the built-in `doctor` becomes a command the consumer
owns, and a failure can carry structured facts instead of prose about them.
Three public types gain a field, hence a **minor** bump pre-1.0. 0.16.0 was
never published; its changes ship in this release.

### Breaking changes

- **`StreamEvent::Error` gains a `data: Option<Value>` field.** The NDJSON
    terminal error carries the same payload as the direct envelope: the same
    failure must not read differently because the caller chose the streaming
    channel. Code matching the variant exhaustively must add `data` or a `..`
    arm.
- **`CommandError` and `ErrorEnvelope` gain a `data` field**
    (`Option<Box<Value>>` and `Option<Value>` respectively — boxed on the error
    type so the `Err` arm of every handler `Result` stays small). Code that
    builds either with a struct literal, or destructures one exhaustively, must
    add `data`. Builder calls (`CommandError::new(...)`,
    `ErrorEnvelope::new(...)`) are unaffected, and an error that never sets a
    payload serializes exactly as before.

### Added

- **`AgentCli::skill()` — help becomes a skill.** Registers a built-in `skill`
    command that prints the CLI as a Claude-Code/Agent-Skills style `SKILL.md`:
    YAML frontmatter plus the envelope contract, every command usage template
    (subcommands indented, raw commands marked), the reserved agent flags, and
    the exit-code and error-code dictionaries — all generated from the live
    command tree, so the file cannot drift from the code. An agent that can run
    the binary once can bootstrap itself from it.
- **`skill --install=<dir>`** writes `<dir>/<name>/SKILL.md`, creating the
    directories, and answers with `{path, bytes, skill_name}` — the absolute
    path and the size of the file, not the markdown a second time. A failed
    write is a `WRITE_FAILED` error envelope, not a panic. An empty, bare
    (`--install` with no value), or unexpanded-tilde (`--install=~/skills`)
    directory is refused with `INVALID_FLAG` before anything is written.
- **`skill` honors `--dry-run`**: it reports the `path` it would write, plus
    `dry_run: true`, and touches no disk.
- **`AgentCli::skill_markdown()`** returns the same document from Rust, for a
    downstream test that pins the generated skill.
- **`audit()` reports `SKILL_NAME_INVALID`** when a CLI that registered
    `.skill()` has a name that is not a slug. The name is both the frontmatter
    `name` a harness matches on and the directory `--install` writes into.
- **`AgentCli::doctor_with(command, checks)`.** `doctor(checks)` hardcodes the
    command — name, description, usage, and therefore *no flags* — so a CLI
    whose `doctor` takes `--profile <name>` had nowhere to declare it:
    unknown-flag rejection reads the usage string as the flag schema and exited
    `2` on the consumer's own flag, and the description had to be smuggled into
    a `root_field`. Now the `Command` is yours — name it, describe it, declare
    flags and `next_actions` on it — and agcli supplies only the handler that
    runs the checks and builds the health report. `doctor(checks)` is sugar over
    it and behaves as before. Without a usage string the framework fills in
    `<cli> <name>`, which keeps flag validation on rather than silently
    disabling it.
- **`Check::with_request(name, run)`.** A check that receives the same
    `CommandRequest` a handler gets, so the flags declared on the `doctor`
    command reach the checks that act on them. `Check::new` checks are unchanged
    and can sit in the same list.
- **`CommandError::data(value)` and `ErrorEnvelope::data(value)`.** A structured
    payload for the failure — the rejected rows and why, the upstream reply, the
    conflicting id — serialized as a `data` key beside `error` and `fix`. Prose
    stays in `message` and `fix`; facts an agent acts on no longer have to be
    encoded into the message text and parsed back out. The key is absent when
    the payload is never set, on both the envelope and the NDJSON `error`
    event.

### Changed

- The root tree's `exit_codes` and `error_codes` dictionaries now come from
    shared tables (`EXIT_CODE_DOCS`, `ERROR_CODE_DOCS`) that the skill renderer
    reads too, so JSON and markdown cannot disagree. `error_codes` gains
    `WRITE_FAILED`.

## [0.15.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.15.0) - 2026-08-19

Two escape hatches for contracts the envelope cannot express: a command that
owes callers raw stdout, and a health check that never ran. Both change public
types, hence a **minor** bump pre-1.0.

### Breaking changes

- **`CheckResult.ok: bool` is now `CheckResult.status: CheckStatus`.** Two
    states could not tell "verified healthy" apart from "never ran", so a
    check with no credentials to test had to lie in one direction or the
    other. Construct with `CheckResult::pass()`, `::pass_with(detail)`,
    `::fail(detail, fix)`, or the new `::skip(reason)`; read with `is_ok()`,
    `failed()`, `skipped()`. Code matching on the `ok` field must move to
    `status`.
- **The `doctor` result gains a `skipped` count, and each check entry gains
    `status`.** Entries keep `ok` (true unless the check failed), so an agent
    reading `ok` still works, but a test asserting the exact shape of a
    `doctor` result will see the new keys.
- **Adding a raw command changes what `main` may print.** A hand-rolled
    `println!("{}", run.to_json()); std::process::exit(run.exit_code())`
    appends an envelope to whatever the raw command already wrote. Replace it
    with `run.finish()`, or guard the print on `!run.is_raw()`. A CLI with no
    raw command is unaffected.

### Added

- **Raw passthrough commands: `Command::raw_handler(...)`.** Some commands owe
    callers a foreign output contract — a ripgrep-compatible `grep` printing
    `path:line:content` and exiting `1` for "no matches", a `cat`, a shim
    around another program. Wrapping those in JSON does not make them
    agent-native, it makes them wrong. A raw handler receives the verbatim
    argv tail as `&[String]`, writes stdout itself, and returns the process
    exit code. Flag parsing, unknown-flag rejection, positional-arity checks,
    the `--dry-run` gate, the reserved `--select`/`--compact`/`--quiet`
    projection, envelope serialization and `next_actions` are all skipped —
    `-C 3`, `-t rust`, `-g '*.rs'`, `--`, and patterns starting with `-`
    arrive untouched. The command still appears in the root tree (marked
    `"raw": true`), in `help`, and in `audit()`. Panics are still caught: the
    handler exits `1` and its `HANDLER_PANIC` envelope goes to stderr, never
    onto the stdout it was writing.
- **`Execution::is_raw()`, `::print()`, and `::finish()`.** `finish()` prints
    the envelope and exits with the typed code — and prints nothing at all when
    a raw command already wrote its own stdout, which a hand-rolled
    `println!("{}", run.to_json())` would corrupt. It is now the recommended
    ending for `main`.
- **`CheckResult::skip(reason)` and `CheckStatus`.** A skipped check keeps
    `healthy` true and never contributes its exit code, so an optional
    subsystem cannot fail a `doctor` run for a caller that does not use it.
- **`Command::is_raw()`** for downstream introspection.
- **`audit()` reports `RAW_COMMAND_HAS_SUBCOMMANDS` and
    `RAW_COMMAND_HAS_HANDLER`** (both error severity): a raw handler consumes
    every token after its own name, so anything declared under it is
    unreachable, and it wins over a `.handler(...)` on the same command, which
    then never runs. `MISSING_USAGE` now covers raw commands too, and
    the reserved-flag-redeclaration warning skips them (the framework parses
    none of their flags).
- **`--json` is documented in the root tree's `agent_flags`.** It was already
    reserved, parsed as a boolean, and ignored — the property a CLI dropping
    its own `--json` flag depends on, since a reserved boolean never consumes
    the token after it (`app brain --json myrepo` keeps `myrepo` as a
    positional). Now an introspecting agent can see that guarantee instead of
    inferring it.

## [0.14.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.14.0) - 2026-07-28

HATEOAS closes the projection gap: list envelopes now carry their own row
schema and a paste-ready `--select` template, so the cheaper projected call
no longer requires the agent to already know the field names. The list
result shape changes, hence a **minor** bump pre-1.0.

### Changed

- **Every `list`/`list_truncated` result gains a `fields` key.** It is emitted
    unconditionally for object rows — including when reserved flags are
    disabled, since `CommandOutput` cannot see that configuration — so any
    downstream test asserting the exact shape of a list result will now see
    the extra key. Pre-1.0, a shape change like this is a **minor** bump.

### Added

- **List results publish their row schema and teach the `--select` call.**
    `CommandOutput::list`/`list_truncated` now emit a `fields` key holding the
    `--select` paths that cover a row — one sorted `items.<key>` dot path per
    distinct top-level key across the items, e.g.
    `["items.id", "items.name"]` — and the framework appends a `next_action`
    templating this exact invocation re-run under `--select`, with those paths
    in the param description. Projection is the largest token saving available
    on a list result, but an agent could only use it after decoding a row to
    guess the field names; now the first response carries both the schema and
    a projection it can paste back unedited. The paths are dot paths on
    purpose: `--select` projects top-level result keys, so a bare `id` would
    miss the rows nested under `items`. The advertisement is skipped when
    `--select` is already in use, under `--quiet`, and when reserved flags or
    the command's reserved projection are disabled; `fields` is omitted for
    empty lists and lists of non-objects.

## [0.13.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.13.0) - 2026-06-11

Agent-ergonomics pass 2, driven by an empirical audit + multi-pass bug hunt of
agcli itself (trail in `agent_ergonomics_audit/`). Closes every silent-fail
class the intent corpus surfaced and fixes two bugs in 0.12.0's hardening.
Behavior changes are a **minor** bump pre-1.0.

### Breaking changes

- **Extra positional arguments are rejected** (when `reserved_flags` is on).
    The framework counts the `<...>` placeholders in the leaf usage string and
    refuses surplus positionals with `EXTRA_ARG` (exit `USAGE`), naming the
    unexpected token(s) and the usage template. Previously `calc add 1 2 3`
    silently dropped the `3` and exited 0 — for a mutating CLI
    (`tool delete <id>` with two ids) that silence is dangerous. Variadic
    usages (`...`) opt out implicitly; opt out per command with
    `Command::allow_extra_args()`.
- **Non-finite floats are rejected by the typed accessors.** `f64::from_str`
    happily parses `"inf"`/`"NaN"`, and serde_json then serializes them as
    `null` — `ok: true` with a corrupted result. `arg_parse`/`flag_parse` now
    return `INVALID_ARG`/`INVALID_FLAG` ("is not finite") instead. Their `T`
    bound gains `+ std::any::Any` (practically every parse target already
    satisfies it).
- **Explicit negation turns reserved booleans off.** `--dry-run=false`,
    `--quiet=0`, `=no`, `=off` now read as *off*; previously presence-testing
    meant `--dry-run=false` *enabled* the dry-run gate. Applies to the
    framework gates and the `req.dry_run()`-family accessors.
- **Root/help/version paths honor `--select`/`--compact`/`--quiet`.** The root
    tree advertises the reserved output flags on every command, but the
    framework-rendered paths ignored them (`calc --quiet` still emitted
    `next_actions`). The root tree of a large CLI is exactly where an agent
    wants `--select=commands`.

### Added

- **`<tool> version` positional alias** — answers like `--version` (unless the
    CLI defines its own `version` command). The `help` alias landed earlier in
    this release; `version` is the same reflex.
- **`error_codes` dictionary in the root help envelope** alongside
    `exit_codes`, so an agent can build retry/branch policy from a single root
    call instead of discovering codes one failure at a time.
- **`audit()` validates the usage-string/parser coupling.** Usage strings are
    simultaneously documentation, the unknown-flag schema, and the arity
    bound — a malformed template silently changes parsing. New findings:
    `UNBALANCED_USAGE_BRACKETS` (error), `USAGE_PROGRAM_MISMATCH` (warning),
    `RESERVED_FLAG_REDECLARED` (warning).
- **Golden-envelope test suite** (`tests/golden_envelopes.rs`) pins the exact
    bytes of every canonical envelope class under `SOURCE_DATE_EPOCH=0`, so
    schema drift is a red test instead of a downstream agent outage.
- **`Command::allow_extra_args()`** — per-command opt-out of the new arity
    check.

### Fixed

- **Usage-declared short value flags were rejected as unknown.** The flag
    schema collected long flags and bracketed short *booleans* but skipped
    short *value* flags, so a usage of `t log [-n <count>]` rejected
    `t log -n 5` — the framework refusing its own advertised affordance — and
    rendered the error as `--n`. Short value flags are now declared, and
    single-character flags render with a single dash everywhere.
- **Out-of-range handler exit codes could escape the panic guard.** The
    0–255 `debug_assert` fired in the framework's *post-handler* path (after
    `catch_unwind`), printing nothing to stdout and exiting 101 — the exact
    non-JSON failure the guard exists to prevent. Envelope-build now masks
    silently; the development-time assert moved into
    `CommandOutput::exit_code` / `CommandError::exit_code`, where it fires
    inside the guard and folds into a structured `HANDLER_PANIC` envelope.
- **"Did you mean" for typo'd flags covers the reserved flags** (`--selct` →
    "Did you mean `--select`?") and resolves equal-distance ties
    deterministically (candidates were previously iterated in `HashSet`
    order).
- **`SOURCE_DATE_EPOCH` edge cases.** Malformed and negative values now clamp
    to 0 per the reproducible-builds convention (previously they silently fell
    back to the wall clock, defeating the pin); values past
    9999-12-31T23:59:59Z clamp to that ceiling so the emitted timestamp stays
    round-trippable through the crate's own parser.

## [0.12.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.12.0) - 2026-06-11

Agent-ergonomics release, driven by an empirical audit of aghealth: two of the
fixes close P0 contract violations (silent unknown flags; `--dry-run` advertised
on commands that mutate anyway). Behavior changes are a **minor** bump pre-1.0.

### Breaking changes

- **Unknown flags are rejected** (when `reserved_flags` is on, the default).
    Any flag not declared in the resolved command path's usage strings — and not
    a framework-reserved flag — returns a structured `UNKNOWN_FLAG` error (exit
    `USAGE`) with a Levenshtein "Did you mean `--limit`?" hint and the full
    declared-flag list. Previously a typo'd flag was silently dropped and the
    command ran with defaults: exit 0, wrong behavior, nothing to learn from.
    Opt out per command with `Command::allow_unknown_flags()`; commands without
    a usage string are exempt (no schema to validate against).
- **`--dry-run` is refused unless the command declares support.** The reserved
    `--dry-run` flag promises "preview without mutating" on every command, but
    the framework cannot know whether a handler honors it. Handlers that read
    `req.dry_run()` must now declare it with `Command::handles_dry_run()`;
    passing `--dry-run` to an unmarked command returns `DRY_RUN_UNSUPPORTED`
    (exit `USAGE`, "Nothing was changed…") instead of silently running — and
    possibly mutating — under a flag that promises a preview.
- **Typed accessor errors now exit `USAGE` (2).** `require_arg`, `arg_parse`,
    and `flag_parse` raised `MISSING_ARG`/`INVALID_ARG`/`INVALID_FLAG` with the
    default exit 1, while conventional CLI helpers used exit 2 for the same
    error class. They now carry `ExitCode::USAGE` so the exit-code dictionary
    is consistent regardless of which layer raised the error.

### Added

- **`<tool> help [command...]` alias** — routes through the same path as
    `--help`/`-h`. It is the first thing many agents guess; it used to be
    `UNKNOWN_COMMAND`.
- **Bare `--version` / `-V`** returns `{name, version}` instead of dumping the
    entire command tree.
- **`exit_codes` dictionary in the root help envelope**, so an agent can branch
    on `$?` without parsing error text.
- **`select_warning` on partial `--select` misses.** A multi-field select with
    one typo'd field used to silently drop the typo; the projection now carries
    a warning naming the unmatched field(s) and the available ones. (Total-miss
    selects already warned.)
- **Group/leaf help `subcommands` is populated again.** A buffer mix-up
    serialized the (empty) path scratch buffer instead of the built docs, so
    `result.subcommands` was always `[]`; `next_actions` masked the bug.
- **`SOURCE_DATE_EPOCH` pins the envelope timestamp** (reproducible-builds
    convention), making same-command runs byte-comparable.

## [0.11.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.11.0) - 2026-06-10

Schema release: the envelope `timestamp` becomes human-readable. Pre-1.0 schema
changes are a **minor** bump.

### Breaking changes

- **`timestamp` is now an RFC 3339 UTC string** (`"2026-06-10T14:42:17Z"`)
    instead of Unix epoch seconds, in both response envelopes and NDJSON
    terminal `result`/`error` events. An epoch integer is unreadable to humans
    and to LLM agents alike; the formatted string means every agcli response
    tells the agent the current date and time directly — no conversion, no
    custom hooks. Consumers parsing `timestamp` as a number must update.
    The Rust API is unchanged: `SuccessEnvelope::timestamp` /
    `ErrorEnvelope::timestamp` remain `u64` epoch seconds (sortable,
    arithmetic-friendly); only the JSON serialization formats it.

### Improvements

- The formatter is hand-rolled (Howard Hinnant's civil-from-days algorithm),
    so the dependency set stays serde/serde_json/tokio. Epoch 0 — the
    never-panic fallback for a pre-1970 wall clock — renders as
    `1970-01-01T00:00:00Z`, preserving the "JSON always" contract.
- Under the `deserialize` feature, `timestamp` is read tolerantly: the new
    RFC 3339 string, a legacy pre-0.11 epoch integer, or anything unparseable
    (which folds to 0, matching the missing-field default).
- Non-terminal stream events' caller-supplied `ts` is now documented as
    RFC 3339 UTC to match the terminal events.

## [0.10.2](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.10.2) - 2026-06-09

Usability release: unknown-command/subcommand errors now self-correct. No schema
change (the `fix` field already existed), so this is a **patch** bump.

### Improvements

- **Unknown-command and unknown-subcommand `fix` text now inlines every valid
    name** instead of a generic "inspect the listed templates" pointer. A
    *semantic* miss — e.g. guessing `list` when the verb is `history`, which no
    edit-distance check would ever relate — is corrected on the first read
    instead of triggering another blind guess.
- **A "Did you mean `<name>`?" nudge is prefixed when the bad token looks like a
    typo** of a real name (case-insensitive Levenshtein distance ≤ 2, gated below
    the candidate's length so an unrelated short word can't match by
    coincidence).

## [0.10.1](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.10.1) - 2026-06-01

Documentation-only release.

### Documentation

- Drop the "Build-machine-specific codegen" (`target-cpu=native`) and PGO
    tuning notes from the README's Performance section. These were niche
    build-tuning tips unrelated to the framework; the section now covers only
    the recommended release profile and allocator guidance.

## [0.10.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.10.0) - 2026-06-01

Correctness and usability fixes from a multi-agent review (26 findings). These
puncture the framework's "JSON always / errors suggest fixes" guarantees at the
edges. Several change the JSON shape (a new `agent_flags` root section, a
`dropped` field on `TruncatedEntries`, exit-code masking), so this is a
**minor** bump (0.9 → 0.10) per the pre-1.0 versioning policy.

### Bug fixes

- **Handler panics no longer break the JSON-always contract.** A panic in
  handler code (unwrap/expect/index/overflow) was unwinding past the envelope
  machinery — empty stdout, raw backtrace on stderr, untyped exit `101`. The
  framework now catches the unwind and emits a structured `HANDLER_PANIC` error
  envelope (exit `1`). stdout stays valid JSON.
- **`--select` no longer silently wipes a result to `{}`.** A bare `--select`,
  an empty value, or a typo'd / no-match field name returned `ok: true` with
  `result: {}`, discarding the handler's real output. It now returns the full
  result plus a `select_warning` listing the available fields.
- **`--select` dot-paths descend element-wise into arrays** (`checks.fix` over
  `{ checks: [ … ] }`) instead of collapsing to `{}`.
- **The built-in `doctor` command is exempt from `--select`/`--compact`
  projection**, so narrowing can never strip the per-check `fix` from an
  unhealthy report.
- **NDJSON emitter self-protects on I/O errors.** A write error now poisons the
  emitter (a retry cannot concatenate onto a partial line) and a terminal event
  is marked terminated *before* flushing (a flush failure can no longer leave
  the stream open to a second terminal event).
- **Exit codes are masked to 0–255**, so the serialized `exit_code` field always
  equals the process status (`exit(256)` no longer reports JSON `256` with shell
  status `0`). A `debug_assert` flags out-of-range codes in development.
- **`doctor` reports the most actionable failing exit code** (a specific typed
  code wins over generic `ERROR`) regardless of check registration order.
- **No more clock panics.** `epoch_secs` and the truncation temp-file naming no
  longer `expect()` on a pre-1970 system clock (they fall back to `0`).
- **`truncate_lines_with_file` floors `max_lines` to 1**, so an agent-supplied
  `--lines=0` returns the tail line instead of an empty inline view marked
  `truncated: true`.
- **Audit flags dead-link `next_action` templates** that lead with a
  placeholder/flag and resolve to no command.
- **`next_action_from_usage`** recognizes short value-flag brackets
  (`[-v <level>]`) and bare optional positionals (`[<optional>]`) as *optional*
  params instead of misclassifying them as required positionals.
- **`Envelope::to_json` serialization fallback** now emits a shape-consistent
  error envelope (`error` object + `exit_code`) instead of a bare `error` string.

### Features

- **Typed argument helpers on `CommandRequest`**: `require_arg(i, name)`,
  `arg_parse::<T>(i, name)`, and `flag_parse::<T>(key)` fold missing/parse
  failures into a `CommandError` with conventional codes (`MISSING_ARG`,
  `INVALID_ARG`, `INVALID_FLAG`) and a generated `fix`.
- **Reserved flags are discoverable.** The self-documenting root tree includes
  an `agent_flags` section, and `agcli::reserved_flag_names()` returns the
  reserved vocabulary programmatically.
- **`TruncatedEntries::dropped`** reports how many head lines were elided; the
  type now documents its tail-of-output and temp-file ownership semantics.
- **`NdjsonEmitter::poisoned()`** accessor.
- **CI-compiled `examples/calc.rs`** so the canonical getting-started example
  can no longer drift from the shipped API.

### Documentation

- Migrate the `CLAUDE.md` "Minimal calculator example" to the real async API
  (it did not compile against the shipped handler/`run_env` signatures).
- Fix two rustdoc warnings (broken `CheckResult` link; public docs linking to
  the private `RESERVED_BOOL_FLAGS`).
- Document the borrow-then-move handler pattern, the opt-in nature of context
  protection, and corrected exit-code serialization docs.

## [0.9.1](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.9.1) - 2026-06-01

Documentation and release-tooling fixes. No code or API changes.

### Documentation

- Rename the confusing "Wokhei-style example" README heading to "Full example"
  (the section points at the `ops` example, not a `wokhei` one).
- Document the tag-driven release process in `CLAUDE.md`.

### CI

- Make the release workflow's publish step idempotent: it now skips cleanly
  when the crate version is already on crates.io instead of failing the run.

## [0.9.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.9.0) - 2026-06-01

Folds the agent-native CLI strategies from
[cli-printing-press](https://github.com/mvanhorn/cli-printing-press) into agcli,
keeping only what a no-bloat framework should own.

### Breaking changes

- **Every envelope now serializes an `exit_code` field.** Both success and
  error envelopes (and the NDJSON `result`/`error` terminal events) carry a
  typed `exit_code` — success defaults `0`, errors default `1`. Consumers that
  asserted an exact envelope key set will see the new field.
- **Framework usage errors now exit `2` (were `1`).** Unknown command, unknown
  subcommand, parse failures, and missing-handler errors return
  `ExitCode::USAGE`. Handler errors without an explicit code still exit `1`.
- **Removed the optional `jemalloc` feature and the `agcli::Jemalloc`
  re-export.** jemalloc targets long-running, concurrent, allocation-heavy
  processes; it does not fit short-lived CLIs and only added a C build step and
  a platform caveat. The default system allocator is the right choice. A binary
  that genuinely needs a custom allocator can add `tikv-jemallocator` directly
  via `#[global_allocator]`.

### Features

- **Typed exit codes.** `ExitCode` taxonomy (`SUCCESS`, `ERROR`, `USAGE`,
  `NOT_FOUND`, `AUTH`, `API`, `RATE_LIMITED`). Opt in from a handler with
  `CommandError::exit_code(...)`; `CommandOutput::exit_code(...)` reports a
  non-zero status while still emitting an `ok: true` envelope.
- **Reserved agent-native output flags**, applied centrally to every command:
  `--select=a,b,c` (field projection over objects and arrays), `--compact`
  (drop null/empty, or a high-gravity allowlist via
  `CommandOutput::compact_fields`), and `--quiet` (drop `next_actions`). Opt
  out per-CLI with `AgentCli::reserved_flags(false)`.
- **Standard flag vocabulary** with typed `CommandRequest` accessors
  (`dry_run`, `assume_yes`, `no_cache`, `no_color`, `wants_stdin`, `compact`,
  `select`) and a free `read_stdin()` helper.
- **Bounded list output**: `CommandOutput::list` / `list_truncated` emit
  `{ items, count, total, truncated, guidance }`.
- **Built-in `doctor` scaffold**: `AgentCli::doctor(checks)` runs `Check`s and
  reports a structured health envelope, surfacing a failing check's typed exit
  code.
- **Static self-audit**: `AgentCli::audit()` validates HATEOAS integrity
  (dangling `next_action` templates), dead-end commands, and missing
  usage/descriptions, returning an `AuditReport`.

### Miscellaneous

- Enable the `tokio` `io-std` feature for the async `read_stdin()` helper.

## [0.8.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.8.0) - 2026-05-28

### Breaking changes

- **Flag parser now accepts `--key value` (space-separated)**, not just
  `--key=value`. A bare `--key` followed by a non-flag token consumes that
  token as the flag's value, matching the HATEOAS `[--flag <value>]` form
  used in `next_actions` templates. `AgentCli::run_argv` walks each command's
  `.usage(...)` string to identify pure-boolean flags (`[--flag]`) so they
  never silently consume positionals.
  - Migration: most callers need no change. If you call `parse_invocation`
    standalone and rely on the old "bare `--flag` is always boolean"
    behavior, switch to `parse_invocation_with_bool_flags` and pass the
    appropriate set.
- **NDJSON emitter is async.** `NdjsonEmitter<W>` now requires
  `W: tokio::io::AsyncWrite + Unpin` and `emit`, `emit_result`, `emit_error`
  are `async fn`s. Replace `emitter.emit(event)?` with
  `emitter.emit(event).await?`.
- **Truncation I/O is async.** `truncate_lines_with_file` and
  `TruncatedEntries::cleanup` are `async fn`s. Add `.await` at call sites.
- **`Command::sync_handler` removed.** Use `.handler(|req, ctx| Box::pin(async
  move { ... }))` directly.
- `tokio` is now a runtime dependency (features `fs`, `io-util`).

### Features

- Add `parse_invocation_with_bool_flags(args, |flag| bool)` for callers that
  want schema-aware parsing outside the `AgentCli` runtime.

### Bug fixes

- Plans/commands submitted via documented `--title "..."` form previously
  surfaced as title `"true"`. The space-separated parser now captures the
  intended value. See the
  [agplan diagnosis](https://github.com/matthiasdebernardini/agplan) for the
  downstream symptom that motivated the release.

## [0.5.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.5.0) - 2026-02-26

### Features

- Add `FlushPolicy` enum for configurable `NdjsonEmitter` flush behavior (Every, Terminal, Never)
- Add criterion benchmark harness covering root invocation, command execution, parse_invocation, truncation, and NDJSON emitter

### Performance

- Eliminate per-subcommand `path.to_vec()` allocation in `subcommand_actions` using push/pop reuse pattern
- Pre-allocate actions vector with `Vec::with_capacity` in `subcommand_actions`

## [0.4.0](https://github.com/matthiasdebernardini/agcli/releases/tag/v0.4.0) - 2026-02-26

### Bug Fixes

- Harden security, correctness, and performance for v0.2.0 ([c033be6](https://github.com/matthiasdebernardini/agcli/commit/c033be65e4f087503851e3fc8ef336d214636b8b))
- Address codex review findings for v0.3.0 ([75f211f](https://github.com/matthiasdebernardini/agcli/commit/75f211f41753324ab5e4c9a33d886c7ef3a1d4c5))

### Features

- Agent-native CLI framework with envelope, streaming, and truncation ([d05358d](https://github.com/matthiasdebernardini/agcli/commit/d05358d39d862d687a451efe2fa8c519b2bebd9c))
- Add retryable, timestamp, and schema_version to envelopes ([14674f6](https://github.com/matthiasdebernardini/agcli/commit/14674f6cedd64ea4629980e36c17d27e79852a19))

### Miscellaneous

- Prepare for crates.io publish ([4c3e5c2](https://github.com/matthiasdebernardini/agcli/commit/4c3e5c23a891612dc77b2f52fff995153c15ea45))


