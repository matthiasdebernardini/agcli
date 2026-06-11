# Changelog

All notable changes to this project will be documented in this file.

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


