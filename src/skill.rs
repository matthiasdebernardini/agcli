//! Render a live CLI definition as an agent skill (`SKILL.md`).
//!
//! Help becomes a skill: the same self-documenting tree the root command
//! returns as JSON is rendered as the markdown-with-frontmatter file an agent
//! harness loads to learn a tool. An agent that can run the CLI once can
//! bootstrap itself from it — no hand-written skill file to drift out of date.
//!
//! Wired up by [`crate::AgentCli::skill`]; readable directly through
//! [`crate::AgentCli::skill_markdown`].

use std::fmt::Write as _;

use crate::cli::{AgentCli, CommandDoc, ERROR_CODE_DOCS, EXIT_CODE_DOCS, RESERVED_FLAG_DOCS};

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
    let _ = writeln!(
        out,
        "- Every `{name}` invocation prints one JSON envelope on stdout: `ok`, `command`, \
         `timestamp` (RFC 3339 UTC), `exit_code`, then `result` on success or `error` plus \
         `fix` on failure, then `next_actions`."
    );
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
    let _ = writeln!(
        out,
        "- Run `{name}` alone for the full command tree. Run `{name} help <command>` for \
         one command.\n"
    );

    let docs = cli.command_docs();
    if !docs.is_empty() {
        out.push_str("## Commands\n\n");
        commands_into(&mut out, &docs, 0);
        out.push('\n');
    }

    if cli.reserved_flags {
        out.push_str("## Agent flags\n\nAccepted on every command:\n\n");
        for (flag, description) in RESERVED_FLAG_DOCS {
            let _ = writeln!(out, "- `{flag}` — {description}");
        }
        out.push('\n');
    }

    out.push_str("## Exit codes\n\n");
    for (code, description) in EXIT_CODE_DOCS {
        let _ = writeln!(out, "- `{code}` — {description}");
    }
    out.push('\n');

    out.push_str("## Error codes\n\nRead from `error.code`:\n\n");
    for (code, description) in ERROR_CODE_DOCS {
        let _ = writeln!(out, "- `{code}` — {description}");
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
        let raw = if doc.raw {
            " (raw: prints the tool's own output, not an envelope)"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "{indent}- `{}` — {}{raw}",
            doc.usage,
            collapse(&doc.description)
        );
        commands_into(out, &doc.subcommands, depth + 1);
    }
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
