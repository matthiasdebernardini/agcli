#![allow(clippy::pedantic)]
//! Agent-native CLI primitives for Rust.
//!
//! This crate enforces an agent-first response model:
//! - JSON envelopes for every command
//! - HATEOAS `next_actions` for follow-up affordances
//! - self-documenting command tree from root/help
//! - NDJSON streaming helpers with terminal result/error events
//! - context-safe truncation helpers for large output
//!
//! # Example
//!
//! ```ignore
//! use agcli::{AgentCli, Command, CommandOutput, ExecutionContext, NextAction};
//! use serde_json::json;
//!
//! #[tokio::main]
//! async fn main() {
//!     let cli = AgentCli::new("ops", "Agent-native operations CLI")
//!         .command(
//!             Command::new("status", "System health")
//!                 .usage("ops status")
//!                 .handler(|_req, _ctx| Box::pin(async move {
//!                     Ok(CommandOutput::new(json!({ "healthy": true })).next_action(
//!                         NextAction::new("ops status", "Re-check health"),
//!                     ))
//!                 })),
//!         );
//!
//!     let mut ctx = ExecutionContext::default();
//!     let run = cli.run_argv_with_context(["ops", "status"], &mut ctx).await;
//!     assert_eq!(run.exit_code(), 0);
//! }
//! ```

mod cli;
mod envelope;
mod stream;
mod truncate;

pub use cli::{
    AgentCli, Command, CommandError, CommandOutput, CommandRequest, Execution, ExecutionContext,
    Invocation, ParseInvocationError, parse_invocation, parse_invocation_with_bool_flags,
};
pub use envelope::{ActionParam, Envelope, ErrorBody, ErrorEnvelope, NextAction, SuccessEnvelope};
pub use stream::{FlushPolicy, LogLevel, NdjsonEmitter, StepStatus, StreamEmitError, StreamEvent};
pub use truncate::{TruncatedEntries, truncate_lines_with_file};

#[cfg(feature = "jemalloc")]
pub use tikv_jemallocator::Jemalloc;
