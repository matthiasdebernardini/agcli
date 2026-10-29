#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

mod agent_cli;
mod audit;
mod clock;
mod codes;
mod command;
mod command_error;
mod command_output;
mod command_request;
mod dispatch;
mod doctor;
mod envelope;
mod execution;
mod invocation;
mod next_action;
mod panic_hook;
mod project;
mod raw;
mod redaction;
mod reserved_flags;
mod select;
mod skill;
mod stream;
mod suggest;
#[cfg(test)]
mod test_support;
mod timestamp;
mod tree;
mod truncate;
mod usage;

pub use agent_cli::AgentCli;
pub use audit::{AuditFinding, AuditReport, AuditSeverity};
pub use codes::{ErrorCode, ExitCode};
pub use command::Command;
pub use command_error::CommandError;
pub use command_output::CommandOutput;
pub use command_request::CommandRequest;
pub use doctor::{Check, CheckResult, CheckStatus};
pub use envelope::{Envelope, ErrorBody, ErrorEnvelope, SuccessEnvelope};
pub use execution::Execution;
pub use invocation::{Invocation, ParseInvocationError};
pub use next_action::{ActionParam, NextAction};
pub use reserved_flags::reserved_flag_names;
#[cfg(feature = "deserialize")]
pub use stream::StreamLine;
pub use stream::{LogLevel, StepStatus, StreamEvent, StreamWriteError, StreamWriter};
pub use timestamp::{ParseTimestampError, Timestamp};
pub use truncate::{TruncatedEntries, truncate_lines_with_file};
