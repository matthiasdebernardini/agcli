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

use serde::Serialize;
use serde_json::{Value, json};

/// Severity of an [`AuditFinding`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[serde(rename_all = "lowercase")]
pub enum AuditSeverity {
    /// Breaks the agent contract (dead next_action, unreachable command).
    Error,
    /// Degrades agent ergonomics but still works (missing usage/description).
    Warning,
}

/// A single problem discovered by [`crate::AgentCli::audit`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
pub struct AuditFinding {
    pub severity: AuditSeverity,
    /// Stable machine code, e.g. `DANGLING_NEXT_ACTION`, `MISSING_USAGE`.
    pub code: String,
    /// Command path the finding is attached to, space separated.
    pub command: String,
    /// Human- and agent-readable explanation.
    pub message: String,
}

/// The result of a static audit pass.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
pub struct AuditReport {
    pub findings: Vec<AuditFinding>,
}

impl AuditReport {
    pub(crate) fn push(
        &mut self,
        severity: AuditSeverity,
        code: impl Into<String>,
        command: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.findings.push(AuditFinding {
            severity,
            code: code.into(),
            command: command.into(),
            message: message.into(),
        });
    }

    /// Number of `Error`-severity findings.
    pub fn error_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == AuditSeverity::Error)
            .count()
    }

    /// Number of `Warning`-severity findings.
    pub fn warning_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == AuditSeverity::Warning)
            .count()
    }

    /// True when there are no `Error`-severity findings. Warnings are
    /// allowed. Ideal for a downstream test: `assert!(cli.audit().is_clean())`.
    pub fn is_clean(&self) -> bool {
        self.error_count() == 0
    }

    /// Render as a JSON value (e.g. to embed in a command result).
    pub fn to_value(&self) -> Value {
        json!({
            "clean": self.is_clean(),
            "errors": self.error_count(),
            "warnings": self.warning_count(),
            "findings": self.findings,
        })
    }
}
