# Working on agcli

agcli is a Rust library crate for building command-line tools that AI agents
operate. Every command answers with a JSON envelope that carries
`next_actions`, errors carry a `fix`, and the command tree documents itself.
It also has NDJSON (newline-delimited JSON) streams, raw passthrough commands,
and built-in `doctor` and `skill` commands.

README.md is the user guide and, through `#![doc = include_str!("../README.md")]`,
the crate documentation. User-facing examples live there and in `examples/`.
This file is for working on the crate itself.

## Layout

`src/`, one module per primary item:

- `lib.rs`: the crate documentation (README.md) and the public re-exports.
- `agent_cli.rs`: `AgentCli`, the CLI definition and its builder (version,
  schema version, clock, reserved flags, root fields), plus the envelope and
  `next_actions` helpers every dispatch path shares.
- `audit.rs`: `AgentCli::audit` and `AuditReport`, `AuditFinding` and
  `AuditSeverity`: static checks of the command tree.
- `clock.rs`: the run's clock, the default `SOURCE_DATE_EPOCH`-or-system clock
  and its validation, and the check that every reading fits a `Timestamp`.
- `codes.rs`: `ExitCode` and `ErrorCode`, and the `EXIT_CODE_DOCS` and
  `ERROR_CODE_DOCS` dictionaries the root tree and the skill publish.
- `command.rs`: `Command`, one node of the tree, its builder, and its one
  handler slot (`Handler::Envelope`, `Stream` or `Raw`).
- `command_error.rs`: `CommandError`, the failure a handler returns and the
  value every error envelope is built from.
- `command_output.rs`: `CommandOutput`, the success payload, including bounded
  lists and their row schema.
- `command_request.rs`: `CommandRequest`, a handler's view of its invocation,
  with the typed argument and flag accessors.
- `dispatch.rs`: `run_env` and `run`, argv in and `Execution` out: parsing,
  command resolution, the validation gates and running a handler.
- `doctor.rs`: the `doctor` built-in: `Check`, `CheckResult`, `CheckStatus`,
  `AgentCli::doctor` and `doctor_with`.
- `envelope.rs`: the envelopes (`SuccessEnvelope`, `ErrorEnvelope`,
  `ErrorBody`, `Envelope`) and their hand-written serialization, in the wire's
  key order.
- `execution.rs`: `Execution`, the outcome of one run, and how it is printed
  without ever panicking.
- `invocation.rs`: `Invocation::parse`, argv into program, flags and
  positionals; `ParseInvocationError` and its per-cause fix; the
  shell-quoted `command` line.
- `next_action.rs`: `NextAction` and `ActionParam`, the templates every
  envelope carries.
- `panic_hook.rs`: the panic hook `run_env` installs, so a panicking handler
  still ends with one `HANDLER_PANIC` envelope.
- `project.rs`: the `--select` and `--compact` projections over JSON values.
- `raw.rs`: raw passthrough commands: finding one in argv before parsing, and
  running it.
- `reserved_flags.rs`: the reserved agent flags and the framework's own
  `--help` and `--version`: names, docs and strict boolean parsing.
- `select.rs`: applying `--select` with its `select_warning`, and the
  `--select` advertisement for list results.
- `skill.rs`: the `skill` built-in, which renders the live tree as `SKILL.md`
  and writes it under `--install`.
- `stream.rs`: NDJSON streaming: `StreamEvent`, `StreamWriter`,
  `StreamWriteError`, and `StreamLine` for decoding.
- `suggest.rs`: did-you-mean suggestions for unknown commands and flags.
- `test_support.rs`: CLIs shared by the unit tests (`cfg(test)` only).
- `timestamp.rs`: `Timestamp`, the second an envelope or event was written,
  with its RFC 3339 `Display` and strict `FromStr`, and
  `ParseTimestampError`.
- `tree.rs`: the self-documenting tree: the root, help and version answers.
- `truncate.rs`: context protection: `truncate_lines_with_file` and
  `TruncatedEntries`.
- `usage.rs`: usage-string parsing: the usage string as the flag schema, the
  arity bound and the `next_action` template.

Elsewhere:

- `tests/`: integration tests, one file per behaviour. `golden_envelopes.rs`
  pins the exact bytes of the canonical envelopes, and `tests/support/` holds
  helpers several test files share.
- `examples/calc.rs` and `examples/ops.rs`: the README's calculator, and a CLI
  with truncated logs, a streaming deploy and a raw command.
- `benches/core.rs`: criterion benchmarks.
- `rules/` (with `sgconfig.yml`): the repo's ast-grep rules.
- `.github/workflows/ci.yml`: the gate on every push to `master` and every pull
  request, plus a job that checks the MSRV (minimum supported Rust version).
- `.github/workflows/release.yml`: the tag-driven release.
- `justfile`: `just gate` runs the CI gate locally.

## Building

Builds go through `mbx`, a cargo wrapper the owner uses: `mbx check`,
`mbx clippy`, `mbx nextest run`, `mbx test`, `mbx doc`,
`mbx run --example calc -- add 3 5`, and `mbx +1.88 check` for another
toolchain. Never run bare `cargo build`, `check`, `clippy` or `test`. Cargo
commands that compile nothing are fine bare: `cargo fmt`, `cargo machete`,
`cargo update`, `cargo package --list`, `cargo metadata`. CI runs the gate
with plain cargo, without the local style scan, and checks the MSRV in a job
of its own.

## The gate

A change is done when every one of these passes:

```bash
cargo fmt --all -- --check
cargo machete
mbx clippy --all-targets --all-features -- -W clippy::all -W clippy::pedantic -D warnings
mbx clippy --all-targets -- -W clippy::all -W clippy::pedantic -D warnings
mbx nextest run --all-features
mbx test --all-features --doc
ast-grep scan --error
bash ~/.claude/skills/google-rust-style/scripts/scan.sh --summary .
! grep -rnE '#!?\[(allow|expect)\(|ast-grep-ignore|rustfmt::skip' src tests examples benches
RUSTDOCFLAGS="-D warnings" mbx doc --no-deps --all-features
mbx +1.88 check --all-targets --all-features
```

- nextest cannot run doctests, so `mbx test --doc` runs them. That includes
  every Rust block in README.md.
- `cargo machete` fails on a dependency nothing uses.
- `ast-grep scan --error` fails on a finding of any severity.
- The Google Rust style scan comes from the google-rust-style skill and must
  report zero warnings.
- The `grep` must find nothing: the crate has no lint suppressions.
- The last line checks the MSRV named by `rust-version` in Cargo.toml.
- `just gate` runs everything except the style scan and the MSRV check.

## Conventions

- **No lint suppressions.** No `#[allow]`, `#![allow]`, `#[expect]`,
  `cfg_attr(.., allow(..))`, `#[rustfmt::skip]` or `// ast-grep-ignore`. Fix the
  code. Raising strictness is fine. If a lint cannot be satisfied without making
  the code worse, stop and ask.
- **No compatibility code.** Breaking changes are welcome. Write no deprecated
  aliases, re-exports under old names, migration code or legacy parsing. When a
  bug sits in code that exists only for compatibility, delete the code.
- **No silent fallbacks.** Invalid input or a failed operation becomes a typed
  error, or an error envelope, never a quiet default.
- **The simplest thing that fully works.** Nothing speculative. Every
  non-trivial behaviour has a test.
- **Tests.** Drive a CLI in-process with `AgentCli::run`. Pin time with
  `AgentCli::clock(|| N)`; never set environment variables in a test. Behaviour
  that needs a real process (the panic hook, live stdout, `SOURCE_DATE_EPOCH`)
  re-executes the test binary: `std::env::current_exe()` plus an environment
  variable that selects a child test. Never `cargo run` from a test. An
  example is tested by `include!`-ing it into a test file, so it has no `//!`
  header: an inner doc comment cannot sit inside the including module.
- **The wire format is a contract.** A change to what the framework prints
  shows up in `tests/golden_envelopes.rs`; treat every diff there as a schema
  change. Keep the README's wire contract, the contract text in `src/skill.rs`,
  `RESERVED_FLAG_DOCS` and the `EXIT_CODE_DOCS`/`ERROR_CODE_DOCS` tables in step
  with it, and record the change in CHANGELOG.md.
- **Docs.** Plain English, present tense, no history ("since v0.8") and no
  compatibility framing. Every public item has rustdoc (`#![warn(missing_docs)]`)
  that says what and why, with `# Errors` and `# Panics` where they apply. Every
  Rust block in README.md is a doctest with hidden `# ` scaffolding: `no_run`
  only for `run_env().await.finish()`, which reads the real argv and exits, and
  `ignore` only with a stated reason.
- **Versioning.** Before 1.0, a breaking change (API, wire format, exit codes, a
  removed feature) is a minor bump; anything else is a patch. Every change gets
  an entry under `## [Unreleased]` in CHANGELOG.md, breaking changes first, each
  with a one-line before and after for downstream code.

## Release

Releases are tag-driven. Pushing a `vX.Y.Z` tag runs
`.github/workflows/release.yml`:

1. **verify**: the tag matches `version` in Cargo.toml, and CHANGELOG.md has a
   `## [X.Y.Z]` heading.
2. **test**: the CI gate and the MSRV check (`ci.yml`).
3. **publish**: `cargo publish`, authenticated by crates.io trusted publishing
   in the `crates-io` environment.
4. **github-release**: a GitHub Release whose notes are that version's section
   of CHANGELOG.md.

**Never run `cargo publish` by hand.** It pre-empts CI: the tag's publish job
then fails with "crate version already exists", and the GitHub Release job,
which depends on it, never runs.

To release `vX.Y.Z`:

1. Land the work in its own commits, with its CHANGELOG entries under
   `## [Unreleased]`.
2. Set `version = "X.Y.Z"` in Cargo.toml and run `cargo update --workspace`,
   which updates agcli's own entry in Cargo.lock.
3. Rename `## [Unreleased]` in CHANGELOG.md to the linked heading every
   release uses:
   `## [X.Y.Z](https://github.com/matthiasdebernardini/agcli/releases/tag/vX.Y.Z) - YYYY-MM-DD`.
   release.yml matches the `## [X.Y.Z]` prefix, so the link does not get in
   its way.
4. Run the gate. `cargo package --list --allow-dirty` shows what the `include`
   list in Cargo.toml ships.
5. Commit as `chore: release vX.Y.Z` and push to `master`.
6. `git tag vX.Y.Z && git push origin vX.Y.Z`. CI does the rest.

The docs carry no version numbers (installation is `cargo add agcli`), so
nothing else needs a bump.
