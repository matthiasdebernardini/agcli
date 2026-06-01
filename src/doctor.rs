//! Built-in `doctor` health-check scaffold.
//!
//! Register a set of [`Check`]s via [`crate::AgentCli::doctor`] and the
//! framework wires up a `doctor` command that runs them and reports a
//! structured health envelope. A failing check drives a typed process exit
//! code (e.g. [`crate::ExitCode::AUTH`]) while still emitting a valid
//! `ok: true` envelope whose `healthy` field and per-check breakdown tell an
//! agent exactly what to fix.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::envelope::ExitCode;

/// Outcome of a single [`Check`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckResult {
    pub ok: bool,
    pub detail: Option<String>,
    pub fix: Option<String>,
}

impl CheckResult {
    /// The check passed, no extra detail.
    pub fn pass() -> Self {
        Self {
            ok: true,
            detail: None,
            fix: None,
        }
    }

    /// The check passed, with an informational detail string.
    pub fn pass_with(detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            detail: Some(detail.into()),
            fix: None,
        }
    }

    /// The check failed. `detail` says what's wrong; `fix` says how to
    /// recover — both surface in the `doctor` envelope.
    pub fn fail(detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            ok: false,
            detail: Some(detail.into()),
            fix: Some(fix.into()),
        }
    }
}

type CheckFuture = Pin<Box<dyn Future<Output = CheckResult> + Send>>;
type CheckFn = dyn Fn() -> CheckFuture + Send + Sync;

/// A single named health check with the exit code to use if it fails.
#[derive(Clone)]
pub struct Check {
    pub(crate) name: String,
    pub(crate) exit_code: i32,
    pub(crate) runner: Arc<CheckFn>,
}

impl Check {
    /// Build a check. `run` is an async closure returning a [`CheckResult`]:
    ///
    /// ```ignore
    /// Check::new("auth", || Box::pin(async {
    ///     if token_present() { CheckResult::pass() }
    ///     else { CheckResult::fail("no token", "Set API_TOKEN") }
    /// })).exit_code(ExitCode::AUTH)
    /// ```
    pub fn new<F>(name: impl Into<String>, run: F) -> Self
    where
        F: Fn() -> CheckFuture + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            exit_code: ExitCode::ERROR,
            runner: Arc::new(run),
        }
    }

    /// Process exit code to surface when this check fails. Defaults to
    /// [`ExitCode::ERROR`].
    pub fn exit_code(mut self, code: i32) -> Self {
        self.exit_code = code;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) async fn run(&self) -> CheckResult {
        (self.runner)().await
    }
}
