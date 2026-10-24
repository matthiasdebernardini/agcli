//! The typed exit and error codes, and the dictionaries of them the root
//! tree and the generated skill publish.

use std::borrow::Cow;
use std::fmt;
use std::num::TryFromIntError;

use serde::Serialize;
use serde_json::Value;

/// A typed process exit code: the status a shell sees in `$?`, and the
/// envelope's `exit_code` field.
///
/// Agents (and shells) branch on the exit status without parsing error
/// text, so each failure class gets its own code: an agent can decide
/// whether to retry, re-authenticate, or fix its arguments. The constants are
/// the framework's taxonomy; [`ExitCode::new`] builds any other status a
/// command means (a raw `grep` returning `1` for "no matches").
///
/// A process status is one byte, so the type is too: every value is a status
/// the process can really exit with, and the serialized `exit_code` (a JSON
/// number) always equals what the shell observes. Converting from an `i32`
/// (`ExitCode::try_from`) fails outside `0..=255` instead of truncating.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[serde(transparent)]
pub struct ExitCode(u8);

impl ExitCode {
    /// Command succeeded.
    pub const SUCCESS: Self = Self(0);
    /// Generic, unclassified failure (the default for handler errors).
    pub const ERROR: Self = Self(1);
    /// Bad invocation: unknown command, bad flag, missing argument.
    pub const USAGE: Self = Self(2);
    /// A requested resource does not exist.
    pub const NOT_FOUND: Self = Self(3);
    /// Authentication or authorization failure.
    pub const AUTH: Self = Self(4);
    /// An upstream/API call failed.
    pub const API: Self = Self(5);
    /// The caller is being rate limited and should back off.
    pub const RATE_LIMITED: Self = Self(7);

    /// The exit code `code`.
    #[must_use]
    pub const fn new(code: u8) -> Self {
        Self(code)
    }

    /// The status byte.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// The status byte as a decimal number, as a shell prints `$?`.
impl fmt::Display for ExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A status as `std::process::Command::status().code()` and
/// `std::process::exit` spell it.
impl TryFrom<i32> for ExitCode {
    type Error = TryFromIntError;

    /// # Errors
    ///
    /// [`TryFromIntError`] when `code` is outside `0..=255`, the range a
    /// process status can carry.
    fn try_from(code: i32) -> Result<Self, Self::Error> {
        u8::try_from(code).map(Self)
    }
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        Self::from(code.0)
    }
}

/// A machine-readable failure class: the envelope's `error.code`.
///
/// The constants are the codes the framework itself raises; a command names
/// its own with [`ErrorCode::new`]. The code is a type of its own, not a
/// string, so it cannot trade places with the message or the fix in
/// [`CommandError::new`](crate::CommandError::new). Serialized as the plain
/// string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[serde(transparent)]
pub struct ErrorCode(Cow<'static, str>);

impl ErrorCode {
    /// The command line could not be parsed: a malformed or repeated flag, or
    /// an argument that is not UTF-8.
    pub const PARSE_ERROR: Self = Self::new("PARSE_ERROR");
    /// No such command.
    pub const UNKNOWN_COMMAND: Self = Self::new("UNKNOWN_COMMAND");
    /// No such subcommand.
    pub const UNKNOWN_SUBCOMMAND: Self = Self::new("UNKNOWN_SUBCOMMAND");
    /// A flag the command does not declare.
    pub const UNKNOWN_FLAG: Self = Self::new("UNKNOWN_FLAG");
    /// More positional arguments than the usage template declares.
    pub const EXTRA_ARG: Self = Self::new("EXTRA_ARG");
    /// A required positional argument is absent.
    pub const MISSING_ARG: Self = Self::new("MISSING_ARG");
    /// A positional argument failed to parse.
    pub const INVALID_ARG: Self = Self::new("INVALID_ARG");
    /// A flag value failed to parse, or a boolean flag got a value that is not
    /// a boolean spelling.
    pub const INVALID_FLAG: Self = Self::new("INVALID_FLAG");
    /// An environment variable the framework reads holds an invalid value.
    pub const INVALID_ENV: Self = Self::new("INVALID_ENV");
    /// The command has neither a handler nor subcommands, so nothing can run.
    pub const MISSING_HANDLER: Self = Self::new("MISSING_HANDLER");
    /// `--dry-run` on a command without a preview mode.
    pub const DRY_RUN_UNSUPPORTED: Self = Self::new("DRY_RUN_UNSUPPORTED");
    /// The command handler panicked.
    pub const HANDLER_PANIC: Self = Self::new("HANDLER_PANIC");
    /// A streaming command could not write its event stream.
    pub const STREAM_WRITE_FAILED: Self = Self::new("STREAM_WRITE_FAILED");
    /// The handler's result could not be serialized.
    pub const SERIALIZATION_FAILED: Self = Self::new("SERIALIZATION_FAILED");
    /// The framework could not write a file the command was asked to produce.
    pub const WRITE_FAILED: Self = Self::new("WRITE_FAILED");
    /// The CLI's name is not a slug, so `skill --install` cannot name the
    /// skill's directory. A bug in the CLI definition.
    pub const SKILL_NAME_INVALID: Self = Self::new("SKILL_NAME_INVALID");

    /// The error code `code`, conventionally `SCREAMING_SNAKE_CASE`.
    #[must_use]
    pub const fn new(code: &'static str) -> Self {
        Self(Cow::Borrowed(code))
    }

    /// The code as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The typed process exit codes the framework defines. Published in the root
/// tree and in the generated skill, keyed by the number a shell sees in `$?`,
/// so an agent can branch on `$?` without parsing error text (see
/// [`ExitCode`]).
pub(crate) const EXIT_CODE_DOCS: &[(ExitCode, &str)] = &[
    (ExitCode::SUCCESS, "success"),
    (ExitCode::ERROR, "error (unclassified failure)"),
    (
        ExitCode::USAGE,
        "usage (unknown command/subcommand/flag, malformed or repeated flag, bad or missing argument or flag value, unsupported --dry-run, invalid SOURCE_DATE_EPOCH)",
    ),
    (
        ExitCode::NOT_FOUND,
        "not_found (a requested resource does not exist)",
    ),
    (
        ExitCode::AUTH,
        "auth (authentication or authorization failure)",
    ),
    (ExitCode::API, "api (an upstream call failed)"),
    (ExitCode::RATE_LIMITED, "rate_limited (back off and retry)"),
];

/// The error codes the framework itself raises, so an agent can build a
/// retry/branch policy from one root call. A command adds codes of its own;
/// every error, whatever its code, also carries `retryable` and `fix`.
pub(crate) const ERROR_CODE_DOCS: &[(ErrorCode, &str)] = &[
    (
        ErrorCode::PARSE_ERROR,
        "the command line could not be parsed (a malformed or repeated flag, or an argument that is not UTF-8); fix says how to repair it",
    ),
    (
        ErrorCode::UNKNOWN_COMMAND,
        "no such command; fix lists valid names",
    ),
    (
        ErrorCode::UNKNOWN_SUBCOMMAND,
        "no such subcommand; fix lists valid names",
    ),
    (
        ErrorCode::UNKNOWN_FLAG,
        "flag not declared by this command; fix lists valid flags",
    ),
    (
        ErrorCode::EXTRA_ARG,
        "more positional arguments than the usage template declares",
    ),
    (
        ErrorCode::MISSING_ARG,
        "a required positional argument is absent",
    ),
    (
        ErrorCode::INVALID_ARG,
        "a positional argument failed to parse",
    ),
    (
        ErrorCode::INVALID_FLAG,
        "a flag value failed to parse; a boolean flag takes no value or one of true/false, 1/0, yes/no, on/off",
    ),
    (
        ErrorCode::INVALID_ENV,
        "an environment variable the framework reads (SOURCE_DATE_EPOCH) is invalid",
    ),
    (
        ErrorCode::MISSING_HANDLER,
        "the command has neither a handler nor subcommands, so nothing can run",
    ),
    (
        ErrorCode::DRY_RUN_UNSUPPORTED,
        "--dry-run passed to a command without a preview mode; nothing was changed",
    ),
    (
        ErrorCode::HANDLER_PANIC,
        "a bug in the command handler, not the invocation",
    ),
    (
        ErrorCode::STREAM_WRITE_FAILED,
        "a streaming command could not write its event stream",
    ),
    (
        ErrorCode::SERIALIZATION_FAILED,
        "the handler's result could not be serialized",
    ),
    (
        ErrorCode::WRITE_FAILED,
        "the framework could not write a file the command was asked to produce",
    ),
    (
        ErrorCode::SKILL_NAME_INVALID,
        "the CLI's name is not a slug, so `skill --install` cannot name the skill's directory; a bug in the CLI definition, not the invocation",
    ),
];

/// Turn `(key, description)` doc entries into a JSON object for the root
/// tree.
pub(crate) fn docs_object<'a>(docs: impl IntoIterator<Item = (String, &'a str)>) -> Value {
    Value::Object(
        docs.into_iter()
            .map(|(key, description)| (key, Value::String(description.to_string())))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_from_i32_fails_outside_a_status_byte() {
        assert_eq!(ExitCode::try_from(0), Ok(ExitCode::SUCCESS));
        assert_eq!(ExitCode::try_from(255), Ok(ExitCode::new(255)));
        assert!(ExitCode::try_from(-1).is_err());
        assert!(ExitCode::try_from(256).is_err());
    }

    #[test]
    fn exit_code_serializes_as_a_number() {
        assert_eq!(
            serde_json::to_value(ExitCode::RATE_LIMITED).expect("serialize"),
            serde_json::json!(7)
        );
        assert_eq!(ExitCode::RATE_LIMITED.get(), 7);
    }

    #[test]
    fn error_code_serializes_and_displays_as_its_string() {
        assert_eq!(
            serde_json::to_value(ErrorCode::UNKNOWN_FLAG).expect("serialize"),
            serde_json::json!("UNKNOWN_FLAG")
        );
        assert_eq!(ErrorCode::new("CUSTOM").to_string(), "CUSTOM");
        assert_eq!(ErrorCode::INVALID_ENV.as_str(), "INVALID_ENV");
    }

    #[test]
    fn every_framework_code_is_documented_once() {
        let mut documented: Vec<&str> = ERROR_CODE_DOCS
            .iter()
            .map(|(code, _)| code.as_str())
            .collect();
        let count = documented.len();
        documented.sort_unstable();
        documented.dedup();
        assert_eq!(documented.len(), count, "a code is documented twice");
        assert!(documented.contains(&"INVALID_ENV"));
        assert!(documented.contains(&"STREAM_WRITE_FAILED"));
    }

    #[test]
    fn exit_code_docs_cover_every_framework_code_in_order() {
        let documented: Vec<ExitCode> = EXIT_CODE_DOCS.iter().map(|(code, _)| *code).collect();
        assert_eq!(
            documented,
            [
                ExitCode::SUCCESS,
                ExitCode::ERROR,
                ExitCode::USAGE,
                ExitCode::NOT_FOUND,
                ExitCode::AUTH,
                ExitCode::API,
                ExitCode::RATE_LIMITED,
            ]
        );
    }
}
