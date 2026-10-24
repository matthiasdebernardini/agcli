//! Render a live CLI definition as an agent skill (`SKILL.md`).
//!
//! Help becomes a skill: the same self-documenting tree the root command
//! returns as JSON is rendered as the markdown-with-frontmatter file an agent
//! harness loads to learn a tool. An agent that can run the CLI once can
//! bootstrap itself from it, with no hand-written skill file to drift out of
//! date.
//!
//! Wired up by [`crate::AgentCli::skill`]; readable directly through
//! [`crate::AgentCli::skill_markdown`].

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{self, Path};

use serde_json::{Map, Value, json};

use crate::agent_cli::AgentCli;
use crate::clock::ClockFn;
use crate::codes::{ERROR_CODE_DOCS, EXIT_CODE_DOCS, ErrorCode, ExitCode};
use crate::command::{Builtin, Command};
use crate::command_error::CommandError;
use crate::execution::{Execution, Output};
use crate::invocation::Invocation;
use crate::next_action::NextAction;
use crate::reserved_flags::{self, RESERVED_FLAG_DOCS};
use crate::tree::CommandDoc;

/// Render `cli` as a `SKILL.md` document.
pub(crate) fn render(cli: &AgentCli) -> String {
    let name = &cli.name;
    let mut out = String::with_capacity(2048);

    out.push_str("---\n");
    let _ = writeln!(out, "name: {}", yaml_scalar(name));
    let _ = writeln!(out, "description: {}", yaml_scalar(&description_line(cli)));
    out.push_str("---\n\n");

    let _ = writeln!(out, "# {name}\n");
    let summary = collapse(&cli.description);
    if !summary.is_empty() {
        let _ = writeln!(out, "{summary}\n");
    }

    out.push_str("## Contract\n\n");
    let _ = write!(
        out,
        "- Every `{name}` invocation prints one JSON envelope on stdout: `ok`, `command`, \
         `timestamp` (RFC 3339 UTC), `exit_code`, then `result` on success or `error` plus \
         `fix` on failure, then `next_actions`."
    );
    if cli.reserved_flags {
        out.push_str(
            " When `--select` names fields that match nothing, a `select_warning` sits \
             beside `result`.",
        );
    }
    out.push('\n');
    out.push_str(
        "- Follow `next_actions`. When an action carries `params`, its `command` is a \
         template: fill every placeholder before you run it. With no `params`, run the \
         command as printed.\n",
    );
    out.push_str(
        "- On failure, `error.code` names the failure class, `fix` says what to do about \
         it, and `error.retryable` says whether the same call can work later.\n",
    );
    out.push_str("- `exit_code` is both the process status and a field. Branch on it.\n");
    let docs = cli.command_docs();
    if any_stream(&docs) {
        out.push_str(
            "- A command marked stream prints NDJSON instead: event lines (`type` `start`, \
             `step`, `progress`, `log` or `event`, each with a `ts`), then exactly one \
             terminal line, the envelope with `type` `result` or `error`. Only the terminal \
             line carries `ok`, and it is always last: read until it.\n",
        );
    }
    if any_raw(&docs) {
        out.push_str(
            "- A command marked raw prints its own output and exits with its own code: no \
             envelope and no `next_actions`. Read its stdout as that tool's output.\n",
        );
    }
    let _ = writeln!(
        out,
        "- Run `{name}` alone for the full command tree. Add `--help` to a command for that \
         command alone. `--help`, `-h`, `--version` and `-V` are answered anywhere on the \
         line, except on a raw command, which owns its own flags.\n"
    );

    if !docs.is_empty() {
        out.push_str("## Commands\n\n");
        commands_into(&mut out, &docs, 0);
        out.push('\n');
    }

    if cli.reserved_flags {
        out.push_str(
            "## Agent flags\n\nAccepted on every command. A boolean flag takes no value, or \
             one of true/false, 1/0, yes/no, on/off; any other value is `INVALID_FLAG`.\n\n",
        );
        for (flag, description) in RESERVED_FLAG_DOCS {
            let _ = writeln!(out, "- `{flag}`: {description}");
        }
        out.push('\n');
    }

    out.push_str("## Exit codes\n\n");
    for (code, description) in EXIT_CODE_DOCS {
        let _ = writeln!(out, "- `{code}`: {description}");
    }
    out.push('\n');

    out.push_str(
        "## Error codes\n\nThe framework's codes, read from `error.code`. A command can add \
         its own; every error also carries `retryable` and `fix`.\n\n",
    );
    for (code, description) in ERROR_CODE_DOCS {
        let _ = writeln!(out, "- `{code}`: {description}");
    }

    out
}

/// The one-line frontmatter description: what the CLI does, plus the trigger
/// sentence an agent harness matches against a user request.
fn description_line(cli: &AgentCli) -> String {
    let name = &cli.name;
    let mut line = collapse(&cli.description);
    if !line.is_empty() {
        if !line.ends_with(['.', '!', '?']) {
            line.push('.');
        }
        line.push(' ');
    }
    let _ = write!(
        line,
        "Use when running the `{name}` CLI or when the user asks to {name} something."
    );
    if let Some(version) = &cli.version {
        let _ = write!(line, " Version {version}.");
    }
    line
}

/// Flatten the command tree into a markdown list, one usage template per line,
/// subcommands indented under their parent.
fn commands_into(out: &mut String, docs: &[CommandDoc], depth: usize) {
    for doc in docs {
        let indent = "  ".repeat(depth);
        let contract = if doc.raw {
            " (raw: prints the tool's own output, not an envelope)"
        } else if doc.stream {
            " (stream: NDJSON event lines, then one terminal envelope line with `type`)"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "{indent}- `{}`: {}{contract}",
            doc.usage,
            collapse(doc.description)
        );
        commands_into(out, &doc.subcommands, depth + 1);
    }
}

/// True when any command in the tree streams.
fn any_stream(docs: &[CommandDoc]) -> bool {
    docs.iter()
        .any(|doc| doc.stream || any_stream(&doc.subcommands))
}

/// True when any command in the tree is raw.
fn any_raw(docs: &[CommandDoc]) -> bool {
    docs.iter().any(|doc| doc.raw || any_raw(&doc.subcommands))
}

/// Fold every run of whitespace into one space. A frontmatter line and a
/// markdown list item are each one line: an author's newline inside a
/// description would otherwise break out of the item it belongs to.
fn collapse(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Emit `value` as a YAML scalar, quoting only when a plain one would parse as
/// something else (a mapping, a comment, an anchor). A CLI description is
/// author-supplied prose: `"name: does X"` must not silently become a nested
/// key.
fn yaml_scalar(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value.contains(": ")
        || value.contains(" #")
        || value.ends_with(':')
        || value.contains('"')
        || value.contains('\'')
        || value.starts_with([
            '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '%', '@', '`',
        ]);
    if !needs_quotes {
        return value.to_string();
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// True when `name` matches `^[a-z0-9]+(-[a-z0-9]+)*$`, the slug shape an
/// agent-skill harness accepts as a frontmatter `name` and a directory name.
fn is_slug(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl AgentCli {
    /// Register a built-in `skill` command that prints this CLI as an agent
    /// skill: a `SKILL.md` document (YAML frontmatter plus markdown) generated
    /// from the live command tree.
    ///
    /// Help becomes a skill. An agent that can run the binary once can learn
    /// the whole surface (the envelope contract, every command template, the
    /// reserved flags, the exit and error codes), and a harness can load the
    /// file directly:
    ///
    /// ```bash
    /// mycli skill                          # markdown in the envelope result
    /// mycli skill --install=.claude/skills # writes .claude/skills/mycli/SKILL.md
    /// ```
    ///
    /// The document is rendered when the command runs, from the finished tree,
    /// so `.skill()` can sit anywhere in the builder chain. `--install` writes
    /// `<dir>/<name>/SKILL.md` and answers with the absolute `path` and its
    /// `bytes` instead of the markdown; under `--dry-run` it reports the path
    /// it would write and writes nothing.
    ///
    /// # Panics
    ///
    /// When the CLI's name is not a slug (lowercase letters and digits, with
    /// single hyphens between them): the name is the skill's frontmatter
    /// `name` and the directory `--install` writes, and a harness accepts
    /// neither otherwise. A mistake in the CLI definition, so it fails the
    /// build of the CLI, not an agent's invocation.
    #[must_use]
    pub fn skill(self) -> Self {
        assert!(
            is_slug(&self.name),
            "CLI name {:?} is not a slug (lowercase letters, digits, and single hyphens between \
             them), so it cannot name a skill",
            self.name
        );
        let usage = format!("{} skill [--install=<dir>]", self.name);
        let mut command = Command::new("skill", "Print this CLI as an agent skill (SKILL.md)")
            .usage(usage)
            .handles_dry_run()
            .default_next_action(NextAction::new(
                self.name.clone(),
                "Inspect the full command tree",
            ));
        command.builtin = Some(Builtin::Skill);
        self.command(command)
    }

    /// This CLI rendered as an agent skill (`SKILL.md`): the same document
    /// the built-in `skill` command returns. Useful in a downstream test that
    /// pins the generated skill, or to write the file from your own code
    /// without registering the command.
    #[must_use]
    pub fn skill_markdown(&self) -> String {
        render(self)
    }

    /// Answer the built-in `skill` command: render the live command tree as a
    /// `SKILL.md` document and, under `--install=<dir>`, write it to
    /// `<dir>/<name>/SKILL.md`.
    pub(crate) fn skill_execution(&self, invocation: &Invocation, clock: &ClockFn) -> Execution {
        let root_action = || NextAction::new(self.name.clone(), "Inspect the full command tree");

        // No `--install`: the document itself is the answer. A bare
        // `--install` reads as the empty directory, so it is refused below
        // instead of writing `./true/<name>/SKILL.md` and reporting success.
        let Some(raw_dir) = invocation.value_flag("install") else {
            let result = json!({
                "markdown": self.skill_markdown(),
                "skill_name": self.name,
            });
            return self.framework_success(invocation, clock, result, vec![root_action()]);
        };

        let dir = raw_dir.trim();
        // `--install=~/skills` never reaches tilde expansion: a shell expands
        // `~` only at the start of a word, not after `=`. A literal `~`
        // directory is never what the caller meant.
        let tilde = dir.split('/').next().is_some_and(|c| c.starts_with('~'));
        if dir.is_empty() || tilde {
            let fix = if tilde {
                format!(
                    "Nothing was written. `~` is not expanded after `=`. Pass an absolute \
                     path: `{} skill --install=/absolute/path`.",
                    self.name
                )
            } else {
                format!(
                    "Nothing was written. Pass the skills directory: \
                     `{} skill --install=<dir>`.",
                    self.name
                )
            };
            return self.error_execution(
                invocation.command_line().to_string(),
                clock,
                CommandError::new(
                    ErrorCode::INVALID_FLAG,
                    format!("--install needs a directory, got {raw_dir:?}"),
                    fix,
                )
                .next_action(root_action()),
            );
        }

        // Absolute, so the reported path means the same thing to a caller with
        // a different working directory. The name is a slug (`skill()` made
        // sure), so the path stays one directory under `dir`.
        let relative = Path::new(dir).join(&self.name).join("SKILL.md");
        let file = match path::absolute(&relative) {
            Ok(file) => file,
            Err(error) => return self.write_failed(invocation, clock, &relative, &error),
        };

        let markdown = self.skill_markdown();
        let bytes = markdown.len();
        // `skill` previews its own write, so `--dry-run` is read whether or
        // not the reserved flags are on; with them off the framework has not
        // checked that its value is a boolean, so that is checked here.
        let dry_run = if let Some(value) = invocation.flag("dry-run") {
            let Some(on) = reserved_flags::parse_bool(value) else {
                return self.error_execution(
                    invocation.command_line().to_string(),
                    clock,
                    reserved_flags::not_a_boolean("dry-run", value).next_action(root_action()),
                );
            };
            on
        } else {
            false
        };
        if !dry_run {
            if let Some(parent) = file.parent()
                && let Err(error) = fs::create_dir_all(parent)
            {
                return self.write_failed(invocation, clock, &file, &error);
            }
            if let Err(error) = fs::write(&file, markdown.as_bytes()) {
                return self.write_failed(invocation, clock, &file, &error);
            }
        }

        // The document is on disk (or would be): returning it inline again
        // would double the envelope for nothing.
        let mut result = Map::new();
        result.insert(
            "path".to_string(),
            Value::String(file.display().to_string()),
        );
        result.insert("bytes".to_string(), Value::from(bytes));
        result.insert("skill_name".to_string(), Value::String(self.name.clone()));
        if dry_run {
            result.insert("dry_run".to_string(), Value::Bool(true));
        }
        self.framework_success(
            invocation,
            clock,
            Value::Object(result),
            vec![root_action()],
        )
    }

    /// The `skill --install` write failed. Filesystem trouble is the caller's
    /// to fix, so it carries [`ExitCode::ERROR`], not a usage code.
    fn write_failed(
        &self,
        invocation: &Invocation,
        clock: &ClockFn,
        file: &Path,
        error: &io::Error,
    ) -> Execution {
        Execution {
            envelope: self
                .build_error_envelope(
                    invocation.command_line().to_string(),
                    clock(),
                    CommandError::new(
                        ErrorCode::WRITE_FAILED,
                        format!("could not write {}: {error}", file.display()),
                        "Check the directory exists and is writable.",
                    )
                    .exit_code(ExitCode::ERROR)
                    .next_action(NextAction::new(
                        self.name.clone(),
                        "Inspect the full command tree",
                    )),
                )
                .into(),
            output: Output::Envelope,
        }
    }
}
