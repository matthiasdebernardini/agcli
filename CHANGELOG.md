# Changelog

All notable changes to this project will be documented in this file.

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


