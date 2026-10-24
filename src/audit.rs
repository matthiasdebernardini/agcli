//! Static self-audit of an [`crate::AgentCli`] definition.
//!
//! This is the framework analog of the "proof-of-behavior" gates used by
//! agent-native CLI generators: instead of validating against an API spec,
//! it validates the command tree against itself. The highest-value check is
//! **HATEOAS integrity** — every statically declared `next_action` must
//! point at a command that actually exists, so an agent following the trail
//! never hits a dead link.
//!
//! Only *statically declared* affordances can be audited: command
//! descriptions, usage strings, and [`crate::Command::default_next_action`]
//! templates. Affordances built dynamically inside a handler at runtime are
//! invisible to a static pass and are out of scope.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde_json::{Value, json};

use crate::agent_cli::AgentCli;
use crate::command::{Builtin, Command};
use crate::reserved_flags;
use crate::skill;
use crate::usage;

/// Severity of an [`AuditFinding`], ordered from least to most severe
/// (`Warning < Error`), so the worst of a report is its `max()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[serde(rename_all = "lowercase")]
pub enum AuditSeverity {
    /// Degrades agent ergonomics but still works (missing usage/description).
    Warning,
    /// Breaks the agent contract (dead `next_action`, unreachable command).
    Error,
}

/// A single problem discovered by [`crate::AgentCli::audit`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[non_exhaustive]
pub struct AuditFinding {
    /// Whether the finding breaks the contract or only degrades it.
    pub severity: AuditSeverity,
    /// Stable machine code, e.g. `DANGLING_NEXT_ACTION`, `MISSING_USAGE`.
    pub code: String,
    /// Command path the finding is attached to, space separated.
    pub command: String,
    /// Human- and agent-readable explanation.
    pub message: String,
}

/// What [`AgentCli::audit`](crate::AgentCli::audit) found, for a test to
/// assert on: `assert!(cli.audit().is_clean())`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[non_exhaustive]
pub struct AuditReport {
    /// Every finding, in the order the tree was walked.
    pub findings: Vec<AuditFinding>,
}

impl AuditReport {
    pub(crate) fn push(
        &mut self,
        severity: AuditSeverity,
        code: &str,
        command: &str,
        message: String,
    ) {
        self.findings.push(AuditFinding {
            severity,
            code: code.to_string(),
            command: command.to_string(),
            message,
        });
    }

    /// Number of `Error`-severity findings.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == AuditSeverity::Error)
            .count()
    }

    /// Number of `Warning`-severity findings.
    #[must_use]
    pub fn warning_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == AuditSeverity::Warning)
            .count()
    }

    /// True when there are no `Error`-severity findings. Warnings are
    /// allowed. Ideal for a downstream test: `assert!(cli.audit().is_clean())`.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.error_count() == 0
    }

    /// Render as a JSON value (e.g. to embed in a command result).
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "clean": self.is_clean(),
            "errors": self.error_count(),
            "warnings": self.warning_count(),
            "findings": self.findings,
        })
    }
}

impl AgentCli {
    /// Statically audit this CLI definition. Validates HATEOAS integrity
    /// (every declared `next_action` resolves to a real command), surfaces
    /// dead-end and undocumented commands, and returns a structured
    /// [`AuditReport`]. Intended for a downstream test:
    /// `assert!(cli.audit().is_clean())`.
    #[must_use]
    pub fn audit(&self) -> AuditReport {
        let mut report = AuditReport::default();
        let mut path_buf: Vec<&str> = Vec::new();
        self.audit_commands(&self.commands, &mut path_buf, &mut report);
        // The CLI name becomes the generated skill's frontmatter `name` *and*
        // the directory `--install` writes into. A harness rejects a name that
        // is not a slug, so the skill would be dead on arrival.
        if self
            .commands
            .get("skill")
            .is_some_and(|c| c.builtin == Some(Builtin::Skill))
            && !skill::is_slug(&self.name)
        {
            report.push(
                AuditSeverity::Error,
                "SKILL_NAME_INVALID",
                "skill",
                format!(
                    "CLI name `{}` is not a slug (lowercase letters, digits, and single \
                     hyphens between them); the generated SKILL.md frontmatter name and its \
                     install directory would both be invalid",
                    self.name
                ),
            );
        }
        report
    }

    fn audit_commands<'a>(
        &self,
        commands: &'a BTreeMap<String, Command>,
        path_buf: &mut Vec<&'a str>,
        report: &mut AuditReport,
    ) {
        for command in commands.values() {
            path_buf.push(&command.name);
            let path = path_buf.join(" ");

            Self::audit_command_shape(command, &path, report);
            if let Some(usage) = &command.usage {
                self.audit_usage(command, usage, &path, report);
            }
            for action in &command.default_next_actions {
                if !self.next_action_resolves(&action.command) {
                    report.push(
                        AuditSeverity::Error,
                        "DANGLING_NEXT_ACTION",
                        &path,
                        format!(
                            "default next_action `{}` does not resolve to a known command",
                            action.command
                        ),
                    );
                }
            }

            self.audit_commands(&command.subcommands, path_buf, report);
            path_buf.pop();
        }
    }

    /// Findings about how `command` is put together: whether it can run,
    /// unreachable subcommands, missing docs.
    fn audit_command_shape(command: &Command, path: &str, report: &mut AuditReport) {
        // The built-in `skill` command is runnable without a handler of
        // its own: the framework dispatches it (see `skill_execution`).
        let runnable = command.handler.is_some() || command.builtin == Some(Builtin::Skill);
        if !runnable && command.subcommands.is_empty() {
            report.push(
                AuditSeverity::Error,
                "DEAD_END_COMMAND",
                path,
                "command has neither a handler nor subcommands; invoking it always errors"
                    .to_string(),
            );
        }
        // A raw command claims the whole argv tail after its own name, so
        // nothing under it can ever be reached.
        if command.is_raw() && !command.subcommands.is_empty() {
            report.push(
                AuditSeverity::Error,
                "RAW_COMMAND_HAS_SUBCOMMANDS",
                path,
                "raw command declares subcommands; a raw handler consumes every \
                 token after its own name, so they are unreachable"
                    .to_string(),
            );
        }
        if command.description.trim().is_empty() {
            report.push(
                AuditSeverity::Warning,
                "EMPTY_DESCRIPTION",
                path,
                "command has an empty description; agents rely on it to choose commands"
                    .to_string(),
            );
        }
        if runnable && command.usage.is_none() {
            report.push(
                AuditSeverity::Warning,
                "MISSING_USAGE",
                path,
                "handler command has no usage string; a framework default is used instead"
                    .to_string(),
            );
        }
    }

    /// Findings about `command`'s usage string. The usage string is not just
    /// documentation — it is the flag schema (`extract_all_flag_names`) and
    /// the positional-arity bound (`usage::positional_arity`). A malformed
    /// template silently changes parsing behavior, so structural problems are
    /// findings.
    fn audit_usage(&self, command: &Command, usage: &str, path: &str, report: &mut AuditReport) {
        let opens = usage.matches('[').count();
        let closes = usage.matches(']').count();
        let lt = usage.matches('<').count();
        let gt = usage.matches('>').count();
        if opens != closes || lt != gt {
            report.push(
                AuditSeverity::Error,
                "UNBALANCED_USAGE_BRACKETS",
                path,
                format!(
                    "usage `{usage}` has unbalanced brackets \
                     ({opens}×'[' vs {closes}×']', {lt}×'<' vs {gt}×'>'); \
                     flag and arity validation parse this template, so it \
                     will misbehave"
                ),
            );
        }
        if usage.split_whitespace().next() != Some(self.name.as_str()) {
            report.push(
                AuditSeverity::Warning,
                "USAGE_PROGRAM_MISMATCH",
                path,
                format!(
                    "usage `{usage}` does not start with the program name \
                     `{}`; agents copy next_action templates verbatim",
                    self.name
                ),
            );
        }
        // Raw commands are exempt: the framework parses none of their
        // flags, so "redeclared" and "intercepted" mean nothing there.
        if command.is_raw() {
            return;
        }
        let mut declared = HashSet::new();
        usage::extract_all_flag_names(usage, &mut declared);
        let mut intercepted: Vec<&str> = declared
            .iter()
            .map(String::as_str)
            .filter(|f| reserved_flags::is_framework_flag(f))
            .collect();
        intercepted.sort_unstable();
        if !intercepted.is_empty() {
            report.push(
                AuditSeverity::Error,
                "INTERCEPTED_FLAG_DECLARED",
                path,
                format!(
                    "usage `{usage}` declares {}; the framework answers --help/-h and \
                     --version/-V itself and parses them as booleans, so the command \
                     never gets the meaning its usage promises",
                    intercepted
                        .iter()
                        .map(|f| usage::flag_display(f))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        if self.reserved_flags {
            let mut redeclared: Vec<&str> = declared
                .iter()
                .map(String::as_str)
                .filter(|f| reserved_flags::reserved_flag_names().contains(f))
                .collect();
            redeclared.sort_unstable();
            if !redeclared.is_empty() {
                report.push(
                    AuditSeverity::Warning,
                    "RESERVED_FLAG_REDECLARED",
                    path,
                    format!(
                        "usage `{usage}` declares framework-reserved flag(s) \
                         {}; the framework already parses and documents them \
                         on every command",
                        redeclared
                            .iter()
                            .map(|f| usage::flag_display(f))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
    }

    /// True if the leading command tokens of a `next_action` template resolve
    /// to a real command path. Parsing stops at the first placeholder/flag
    /// token (`<arg>`, `[--flag]`, `-x`, `key=value`).
    ///
    /// A bare program-name reference (or an empty template) is the valid root
    /// affordance and resolves. But a template that — after dropping an optional
    /// leading program name — leads straight into a placeholder/flag matches
    /// *zero* command tokens and names no runnable command, so it is rejected
    /// (it would be a dead link for an agent following the trail).
    fn next_action_resolves(&self, template: &str) -> bool {
        let mut tokens = template.split_whitespace().peekable();
        if tokens.peek() == Some(&self.name.as_str()) {
            tokens.next();
        }
        let mut commands = &self.commands;
        let mut matched_command = false;
        let mut saw_token = false;
        for token in tokens {
            saw_token = true;
            if token.starts_with('<')
                || token.starts_with('[')
                || token.starts_with('-')
                || token.contains('=')
            {
                break;
            }
            match commands.get(token) {
                Some(found) => {
                    commands = &found.subcommands;
                    matched_command = true;
                }
                None => return false,
            }
        }
        // Resolved if it matched a real command, or it's the bare-root
        // affordance (no tokens after the optional program name). A template
        // that had tokens but matched no command (leading placeholder/flag) is
        // a dead link.
        matched_command || !saw_token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::codes::ExitCode;
    use crate::command_output::CommandOutput;
    use crate::next_action::NextAction;
    use crate::test_support::sample_cli;

    #[cfg(feature = "deserialize")]
    #[test]
    fn audit_report_round_trips_strictly() {
        let mut report = AuditReport::default();
        report.push(
            AuditSeverity::Error,
            "DANGLING_NEXT_ACTION",
            "app ls",
            "dead link".to_string(),
        );
        report.push(
            AuditSeverity::Warning,
            "MISSING_USAGE",
            "app",
            "no usage".to_string(),
        );
        let json = serde_json::to_string(&report).expect("serialize");
        let decoded: AuditReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, report);

        let missing = r#"{"findings":[{"severity":"error","code":"X","command":"app"}]}"#;
        assert!(serde_json::from_str::<AuditReport>(missing).is_err());
    }

    // --- Static audit ---

    #[test]
    fn audit_clean_cli_is_clean() {
        let cli = sample_cli();
        let report = cli.audit();
        assert!(
            report.is_clean(),
            "unexpected findings: {:?}",
            report.findings
        );
    }

    #[test]
    fn audit_flags_dangling_next_action() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("a", "Command A")
                .usage("app a")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) })
                .default_next_action(NextAction::new("app nonexistent", "go nowhere")),
        );
        let report = cli.audit();
        assert!(!report.is_clean());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == "DANGLING_NEXT_ACTION")
        );
    }

    #[test]
    fn audit_flags_dead_end_command() {
        let cli = AgentCli::new("app", "x").command(Command::new("orphan", "no handler, no subs"));
        let report = cli.audit();
        assert!(report.findings.iter().any(|f| f.code == "DEAD_END_COMMAND"));
        assert!(!report.is_clean());
    }

    // --- Audit: leading placeholder/flag next_action is a dead link ---

    #[test]
    fn audit_flags_next_action_leading_with_placeholder() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("a", "Command A")
                .usage("app a")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) })
                .default_next_action(NextAction::new("<id>", "names no command")),
        );
        let report = cli.audit();
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == "DANGLING_NEXT_ACTION")
        );
    }

    #[test]
    fn audit_allows_bare_root_next_action() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("a", "Command A")
                .usage("app a")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) })
                .default_next_action(NextAction::new("app", "Inspect the root tree")),
        );
        assert!(cli.audit().is_clean(), "{:?}", cli.audit().findings);
    }

    // --- audit() validates the usage-string/parser coupling (R11) ---

    #[test]
    fn audit_flags_unbalanced_usage_brackets() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("a", "Command A")
                .usage("app a [--broken <x>")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
        );
        assert!(
            cli.audit()
                .findings
                .iter()
                .any(|f| f.code == "UNBALANCED_USAGE_BRACKETS")
        );
    }

    #[test]
    fn audit_flags_usage_program_mismatch() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("a", "Command A")
                .usage("otherprog a <x>")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
        );
        assert!(
            cli.audit()
                .findings
                .iter()
                .any(|f| f.code == "USAGE_PROGRAM_MISMATCH")
        );
    }

    #[test]
    fn audit_flags_reserved_flag_redeclared() {
        let cli = AgentCli::new("app", "x").command(
            Command::new("a", "Command A")
                .usage("app a [--quiet]")
                .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
        );
        assert!(
            cli.audit()
                .findings
                .iter()
                .any(|f| f.code == "RESERVED_FLAG_REDECLARED")
        );
    }

    #[test]
    fn audit_flags_a_usage_declaring_an_intercepted_flag() {
        for usage in [
            "app a [--help]",
            "app a [-h]",
            "app a [--version <v>]",
            "app a [-V <level>]",
        ] {
            let cli = AgentCli::new("app", "x").command(
                Command::new("a", "Command A")
                    .usage(usage)
                    .handler(|_req| async move { Ok(CommandOutput::new(json!({}))) }),
            );
            let report = cli.audit();
            let finding = report
                .findings
                .iter()
                .find(|f| f.code == "INTERCEPTED_FLAG_DECLARED")
                .unwrap_or_else(|| panic!("{usage}: {:?}", report.findings));
            assert_eq!(finding.severity, AuditSeverity::Error, "{usage}");
            assert!(!report.is_clean(), "{usage}");
            // Reported whether or not the agent flags are on.
            assert!(
                cli.clone()
                    .without_reserved_flags()
                    .audit()
                    .findings
                    .iter()
                    .any(|f| f.code == "INTERCEPTED_FLAG_DECLARED"),
                "{usage}"
            );
        }
        // A raw command owns its `-h`: nothing is intercepted there.
        let raw = AgentCli::new("app", "x").command(
            Command::new("grep", "Search")
                .usage("app grep [-h] [args...]")
                .raw_handler(|_args| async move { ExitCode::SUCCESS }),
        );
        assert!(raw.audit().is_clean(), "{:?}", raw.audit().findings);
    }
}
