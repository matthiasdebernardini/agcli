//! Built-in `doctor` health-check scaffold.
//!
//! Register a set of [`Check`]s via [`crate::AgentCli::doctor`] and the
//! framework wires up a `doctor` command that runs them and reports a
//! structured health envelope. A failing check drives a typed process exit
//! code (e.g. [`crate::ExitCode::AUTH`]) while still emitting a valid
//! `ok: true` envelope whose `healthy` field and per-check breakdown tell an
//! agent exactly what to fix.
//!
//! A check has three outcomes, not two. [`CheckResult::skip`] reports that the
//! check never ran — no credentials configured, an optional dependency absent,
//! a platform the check does not apply to. A skipped check is not a failure: it
//! leaves `healthy` true and never drives the exit code. Folding "did not run"
//! into "passed" would tell an agent something was verified when nothing was.
//!
//! A check may also read the invocation that triggered it.
//! [`Check::with_request`] hands the check the same [`CommandRequest`] a
//! handler gets, so a `doctor` command that declares its own flags — a
//! `--profile <name>` selecting which credentials to verify — can act on them
//! instead of always checking the default. Declare those flags on the
//! [`crate::Command`] you pass to [`crate::AgentCli::doctor_with`]; the usage
//! string is the flag schema.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::cli::CommandRequest;
use crate::envelope::ExitCode;

/// The three outcomes of a [`Check`]. Serialized into the `doctor` report as
/// the lowercase `status` string on each check entry.
///
/// `#[non_exhaustive]`: a fourth outcome — a warning tier, say — would
/// otherwise break every downstream `match`. Add a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckStatus {
    /// The check ran and what it verifies is healthy.
    Pass,
    /// The check ran and found a problem. Drives `healthy: false` and the
    /// check's [`Check::exit_code`].
    Fail,
    /// The check did not run — it does not apply here, or a precondition for
    /// running it is absent. Neither healthy nor broken.
    Skip,
}

impl CheckStatus {
    /// The wire spelling used in the `doctor` envelope.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }
}

/// Outcome of a single [`Check`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckResult {
    pub status: CheckStatus,
    pub detail: Option<String>,
    pub fix: Option<String>,
}

impl CheckResult {
    /// The check passed, no extra detail.
    pub fn pass() -> Self {
        Self {
            status: CheckStatus::Pass,
            detail: None,
            fix: None,
        }
    }

    /// The check passed, with an informational detail string.
    pub fn pass_with(detail: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Pass,
            detail: Some(detail.into()),
            fix: None,
        }
    }

    /// The check failed. `detail` says what's wrong; `fix` says how to
    /// recover — both surface in the `doctor` envelope.
    pub fn fail(detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Fail,
            detail: Some(detail.into()),
            fix: Some(fix.into()),
        }
    }

    /// The check did not run. `reason` says why — it lands in `detail`.
    ///
    /// A skipped check keeps the report healthy and never sets the exit code,
    /// so an optional subsystem cannot fail a `doctor` run for a caller that
    /// does not use it.
    pub fn skip(reason: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Skip,
            detail: Some(reason.into()),
            fix: None,
        }
    }

    /// True unless the check failed. A skipped check is not a failure.
    pub fn is_ok(&self) -> bool {
        !matches!(self.status, CheckStatus::Fail)
    }

    /// True only for [`CheckStatus::Fail`].
    pub fn failed(&self) -> bool {
        matches!(self.status, CheckStatus::Fail)
    }

    /// True only for [`CheckStatus::Skip`].
    pub fn skipped(&self) -> bool {
        matches!(self.status, CheckStatus::Skip)
    }
}

type CheckFuture = Pin<Box<dyn Future<Output = CheckResult> + Send>>;
type CheckFn = dyn Fn() -> CheckFuture + Send + Sync;
type RequestCheckFn = dyn for<'a> Fn(&'a CommandRequest<'a>) -> Pin<Box<dyn Future<Output = CheckResult> + Send + 'a>>
    + Send
    + Sync;

/// How a [`Check`] is invoked: on its own, or with the request that triggered
/// the `doctor` run.
#[derive(Clone)]
enum CheckRunner {
    Bare(Arc<CheckFn>),
    WithRequest(Arc<RequestCheckFn>),
}

/// A single named health check with the exit code to use if it fails.
#[derive(Clone)]
pub struct Check {
    pub(crate) name: String,
    pub(crate) exit_code: i32,
    runner: CheckRunner,
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
            runner: CheckRunner::Bare(Arc::new(run)),
        }
    }

    /// Build a check that reads the invocation. `run` receives the same
    /// [`CommandRequest`] a handler gets — flags, positionals, `dry_run()` —
    /// so the check can verify what the caller asked about:
    ///
    /// ```ignore
    /// Check::with_request("profile", |req| {
    ///     let profile = req.flag("profile").map(str::to_string);
    ///     Box::pin(async move {
    ///         match load_profile(profile.as_deref()) {
    ///             Some(p) => CheckResult::pass_with(format!("profile {p} resolves")),
    ///             None => CheckResult::fail("no such profile", "Run `app login`"),
    ///         }
    ///     })
    /// })
    /// ```
    ///
    /// The flags a check reads must be declared in the usage string of the
    /// [`crate::Command`] given to [`crate::AgentCli::doctor_with`], or the
    /// framework rejects them as unknown before any check runs.
    pub fn with_request<F>(name: impl Into<String>, run: F) -> Self
    where
        F: for<'a> Fn(
                &'a CommandRequest<'a>,
            ) -> Pin<Box<dyn Future<Output = CheckResult> + Send + 'a>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            name: name.into(),
            exit_code: ExitCode::ERROR,
            runner: CheckRunner::WithRequest(Arc::new(run)),
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

    pub(crate) async fn run(&self, req: &CommandRequest<'_>) -> CheckResult {
        match &self.runner {
            CheckRunner::Bare(run) => run().await,
            CheckRunner::WithRequest(run) => run(req).await,
        }
    }
}
