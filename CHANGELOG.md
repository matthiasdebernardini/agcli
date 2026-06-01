# Changelog

All notable changes to this project will be documented in this file.

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


