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
//! check never ran: no credentials configured, an optional dependency absent,
//! a platform the check does not apply to. A skipped check is not a failure: it
//! leaves `healthy` true and never drives the exit code. Folding "did not run"
//! into "passed" would tell an agent something was verified when nothing was.
//!
//! Every check reads the invocation that triggered it: [`Check::new`] hands
//! the check the same [`CommandRequest`] a handler gets, so a `doctor` command
//! that declares its own flags (a `--profile <name>` selecting which
//! credentials to verify) can act on them instead of always checking the
//! default. Declare those flags on the
//! [`crate::Command`] you pass to [`crate::AgentCli::doctor_with`]; the usage
//! string is the flag schema.

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::agent_cli::AgentCli;
use crate::codes::ExitCode;
use crate::command::{BoxFuture, Builtin, Command};
use crate::command_output::CommandOutput;
use crate::command_request::CommandRequest;

/// The three outcomes of a [`Check`]. Serialized into the `doctor` report as
/// the lowercase `status` string on each check entry.
///
/// Exhaustive, like the other enums on the wire: a new outcome is a change
/// to the report's contract, and a `match` should be made to notice it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CheckStatus {
    /// The check ran and what it verifies is healthy.
    Pass,
    /// The check ran and found a problem. Drives `healthy: false` and the
    /// check's [`Check::exit_code`].
    Fail,
    /// The check did not run: it does not apply here, or a precondition for
    /// running it is absent. Neither healthy nor broken.
    Skip,
}

impl CheckStatus {
    /// The wire spelling used in the `doctor` envelope.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }
}

/// Outcome of a single [`Check`]. Build one with [`CheckResult::pass`],
/// [`CheckResult::pass_with`], [`CheckResult::fail`] or [`CheckResult::skip`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CheckResult {
    /// Whether the check passed, failed or did not run. Serialized as the
    /// entry's `status`.
    pub status: CheckStatus,
    /// What the check found, or why it was skipped. Serialized as the entry's
    /// `detail`, and left out when `None`.
    pub detail: Option<String>,
    /// How to repair a failure. Set only by [`CheckResult::fail`], and
    /// serialized as the entry's `fix`.
    pub fix: Option<String>,
}

impl CheckResult {
    /// The check passed, no extra detail.
    #[must_use]
    pub fn pass() -> Self {
        Self {
            status: CheckStatus::Pass,
            detail: None,
            fix: None,
        }
    }

    /// The check passed, with an informational detail string.
    #[must_use]
    pub fn pass_with(detail: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Pass,
            detail: Some(detail.into()),
            fix: None,
        }
    }

    /// The check failed. `detail` says what is wrong and `fix` says how to
    /// recover; both surface in the `doctor` report.
    #[must_use]
    pub fn fail(detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Fail,
            detail: Some(detail.into()),
            fix: Some(fix.into()),
        }
    }

    /// The check did not run. `reason` says why, and lands in `detail`.
    ///
    /// A skipped check keeps the report healthy and never sets the exit code,
    /// so an optional subsystem cannot fail a `doctor` run for a caller that
    /// does not use it.
    #[must_use]
    pub fn skip(reason: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Skip,
            detail: Some(reason.into()),
            fix: None,
        }
    }

    /// True unless the check failed. A skipped check is not a failure.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        !matches!(self.status, CheckStatus::Fail)
    }

    /// True only for [`CheckStatus::Fail`].
    #[must_use]
    pub fn is_failed(&self) -> bool {
        matches!(self.status, CheckStatus::Fail)
    }

    /// True only for [`CheckStatus::Skip`].
    #[must_use]
    pub fn is_skipped(&self) -> bool {
        matches!(self.status, CheckStatus::Skip)
    }
}

type CheckFn = dyn Fn(CommandRequest) -> BoxFuture<CheckResult> + Send + Sync;

/// One named health check for the `doctor` command: an async closure that
/// returns a [`CheckResult`], and the exit code a failure sets.
///
/// Build it with [`Check::new`] and register it with
/// [`AgentCli::doctor`](crate::AgentCli::doctor) or
/// [`AgentCli::doctor_with`](crate::AgentCli::doctor_with).
#[derive(Clone)]
pub struct Check {
    pub(crate) name: String,
    pub(crate) exit_code: ExitCode,
    run: Arc<CheckFn>,
}

impl fmt::Debug for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Check")
            .field("name", &self.name)
            .field("exit_code", &self.exit_code)
            .finish_non_exhaustive()
    }
}

impl Check {
    /// Build a check called `name`. `run` receives the same
    /// [`CommandRequest`] a handler gets (flags, positionals, `dry_run()`) and
    /// returns a future resolving to a [`CheckResult`], so the check can
    /// verify what the caller asked about:
    ///
    /// ```
    /// use agcli::{Check, CheckResult, ExitCode};
    ///
    /// let profile = Check::new("profile", |req| async move {
    ///     match req.flag("profile") {
    ///         Some(name) => CheckResult::pass_with(format!("profile {name} resolves")),
    ///         None => CheckResult::fail("no profile given", "Pass --profile=<name>"),
    ///     }
    /// })
    /// .exit_code(ExitCode::AUTH);
    /// # let _ = profile;
    /// ```
    ///
    /// The flags a check reads must be declared in the usage string of the
    /// [`crate::Command`] given to [`crate::AgentCli::doctor_with`], or the
    /// framework rejects them as unknown before any check runs.
    #[must_use]
    pub fn new<F, Fut>(name: impl Into<String>, run: F) -> Self
    where
        F: Fn(CommandRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = CheckResult> + Send + 'static,
    {
        Self {
            name: name.into(),
            exit_code: ExitCode::ERROR,
            run: Arc::new(move |req| Box::pin(run(req))),
        }
    }

    /// Process exit code to surface when this check fails. Defaults to
    /// [`ExitCode::ERROR`].
    #[must_use]
    pub fn exit_code(mut self, code: ExitCode) -> Self {
        self.exit_code = code;
        self
    }

    /// The name the report's entry for this check carries.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) async fn run(&self, req: CommandRequest) -> CheckResult {
        (self.run)(req).await
    }
}

impl AgentCli {
    /// Register a built-in `doctor` command that runs `checks` and reports a
    /// structured health envelope `{ healthy, skipped, checks: [...] }`. When
    /// any check fails the command still returns an `ok: true` envelope (the
    /// report succeeded) but carries the failing check's exit code so a shell
    /// or agent sees a non-zero status.
    ///
    /// Each check entry carries its `name` and `status` (`pass`, `fail` or
    /// `skip`), plus `detail` and `fix` when the check gave them. A skipped
    /// check ([`CheckResult::skip`]) did not run: it leaves `healthy` true and
    /// never contributes an exit code, so an optional subsystem cannot fail
    /// the run for a caller that does not use it. Checks run in order, and
    /// `--select` and `--compact` never apply to the report, so a projection
    /// cannot hide a fix. See [`Check`] and [`crate::CheckResult`].
    ///
    /// The command is fixed: named `doctor`, described "Run environment health
    /// checks", usage `<cli> doctor`, no flags of its own. To name it, describe
    /// it, or give it flags, build the [`Command`] yourself and pass it to
    /// [`AgentCli::doctor_with`].
    #[must_use]
    pub fn doctor(self, checks: Vec<Check>) -> Self {
        self.doctor_with(
            Command::new("doctor", "Run environment health checks"),
            checks,
        )
    }

    /// Register a `doctor` command from a [`Command`] you supply, running
    /// `checks` under it.
    ///
    /// Same report and same exit-code rules as [`AgentCli::doctor`]; this
    /// version lets the command be yours. The name, the description, the
    /// usage string (and so the declared flags), the `next_actions`: all come
    /// from `command`. agcli supplies only the handler, which runs the checks
    /// and builds the health envelope; any handler already on `command` is
    /// replaced.
    ///
    /// Declared flags reach the checks through the [`CommandRequest`] each
    /// [`Check`] receives. Declaring them is what makes them legal:
    /// unknown-flag rejection reads the usage string as the flag schema, so
    /// `app --profile ci doctor` fails with `UNKNOWN_FLAG` until `--profile`
    /// appears there:
    ///
    /// ```
    /// use agcli::{AgentCli, Check, CheckResult, Command, ExitCode};
    ///
    /// let doctor = Command::new("doctor", "Check the active or named profile")
    ///     .usage("app doctor [--profile=<name>]");
    /// let cli = AgentCli::new("app", "…").doctor_with(doctor, vec![
    ///     Check::new("profile", |req| async move {
    ///         let profile = req.flag("profile").unwrap_or("active");
    ///         CheckResult::pass_with(format!("profile {profile} resolves"))
    ///     })
    ///     .exit_code(ExitCode::AUTH),
    /// ]);
    /// # let _ = cli;
    /// ```
    ///
    /// A usage string is optional. Without one the framework fills in
    /// `<cli> <name>`, which keeps flag validation on (declaring no flags)
    /// rather than silently switching it off for the whole command.
    #[must_use]
    pub fn doctor_with(self, mut command: Command, checks: Vec<Check>) -> Self {
        if command.usage.is_none() {
            let usage = format!("{} {}", self.name, command.name);
            command = command.usage(usage);
        }
        let checks = Arc::new(checks);
        let mut command = command.handler(move |req| {
            let checks = Arc::clone(&checks);
            async move {
                let mut entries = Vec::with_capacity(checks.len());
                let mut healthy = true;
                let mut skipped = 0usize;
                let mut fail_exit: Option<ExitCode> = None;
                for check in checks.iter() {
                    let result = check.run(req.clone()).await;
                    if result.is_failed() {
                        healthy = false;
                        // Prefer a specific (non-ERROR) exit code over the
                        // generic ERROR so the shell sees the most
                        // actionable failure class regardless of the order
                        // checks were registered in.
                        fail_exit = Some(match fail_exit {
                            None => check.exit_code,
                            Some(prev) if prev == ExitCode::ERROR => check.exit_code,
                            Some(prev) => prev,
                        });
                    }
                    if result.is_skipped() {
                        skipped += 1;
                    }
                    let mut entry = Map::new();
                    entry.insert("name".to_string(), json!(check.name()));
                    entry.insert("status".to_string(), json!(result.status.as_str()));
                    if let Some(detail) = result.detail {
                        entry.insert("detail".to_string(), json!(detail));
                    }
                    if let Some(fix) = result.fix {
                        entry.insert("fix".to_string(), json!(fix));
                    }
                    entries.push(Value::Object(entry));
                }
                let mut output = CommandOutput::new(json!({
                    "healthy": healthy,
                    "skipped": skipped,
                    "checks": entries,
                }));
                if !healthy {
                    output = output.exit_code(fail_exit.unwrap_or(ExitCode::ERROR));
                }
                Ok(output)
            }
        });
        // Never narrow the doctor report: `--select`/`--compact` must not be
        // able to strip the per-check `fix` strings from an unhealthy result.
        command.builtin = Some(Builtin::Doctor);
        self.command(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::envelope::Envelope;

    // --- Doctor ---

    #[tokio::test]
    async fn doctor_reports_healthy_when_all_checks_pass() {
        let cli = AgentCli::new("app", "x").doctor(vec![Check::new("ping", |_req| async {
            CheckResult::pass_with("ok")
        })]);
        let run = cli.run(["app", "doctor"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["healthy"], json!(true));
        assert_eq!(run.exit_code(), ExitCode::SUCCESS);
    }

    #[tokio::test]
    async fn doctor_failing_check_sets_exit_code() {
        let cli = AgentCli::new("app", "x").doctor(vec![
            Check::new("auth", |_req| async {
                CheckResult::fail("no token", "Set API_TOKEN")
            })
            .exit_code(ExitCode::AUTH),
        ]);
        let run = cli.run(["app", "doctor"]).await;
        // Still an ok:true envelope (the report succeeded), but exit code 4.
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["healthy"], json!(false));
        assert_eq!(run.exit_code(), ExitCode::AUTH);
    }

    #[tokio::test]
    async fn doctor_prefers_specific_exit_code_over_generic_error() {
        // A generic-ERROR check registered before a typed check must not mask
        // the more actionable code: priority picks AUTH over ERROR regardless
        // of registration order.
        let cli = AgentCli::new("app", "x").doctor(vec![
            Check::new("connectivity", |_req| async {
                CheckResult::fail("down", "Check the network")
            }),
            Check::new("auth", |_req| async {
                CheckResult::fail("no token", "Set API_TOKEN")
            })
            .exit_code(ExitCode::AUTH),
        ]);
        let run = cli.run(["app", "doctor"]).await;
        assert_eq!(run.exit_code(), ExitCode::AUTH);
    }

    #[tokio::test]
    async fn doctor_select_does_not_strip_checks_and_fix() {
        // Reserved projection is exempt for doctor: narrowing must never hide
        // the per-check `fix` on an unhealthy report.
        let cli = AgentCli::new("app", "x").doctor(vec![
            Check::new("auth", |_req| async {
                CheckResult::fail("no token", "Set API_TOKEN")
            })
            .exit_code(ExitCode::AUTH),
        ]);
        let run = cli.run(["app", "doctor", "--select=healthy"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.result["checks"].is_array());
        assert_eq!(envelope.result["checks"][0]["fix"], json!("Set API_TOKEN"));
        assert_eq!(run.exit_code(), ExitCode::AUTH);
    }
}
