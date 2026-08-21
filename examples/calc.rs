//! Minimal agent-native calculator: `calc add <a> <b>` and `calc sub <a> <b>`.
//!
//! This is the canonical getting-started example. It is compiled by CI (it is
//! an `examples/` target) so it cannot drift from the shipped async API.
//!
//! ```bash
//! cargo run --example calc            # root command — self-documenting tree
//! cargo run --example calc -- add 3 5
//! cargo run --example calc -- sub 10 4
//! cargo run --example calc -- add foo bar   # typed-error envelope
//! cargo run --example calc -- skill         # this CLI as a SKILL.md
//! ```

use agcli::{ActionParam, AgentCli, Command, CommandError, CommandOutput, ExitCode, NextAction};
use serde_json::json;

/// `serde_json` renders non-finite floats as `null`, which would corrupt the
/// result while still reporting `ok: true` — so overflow is a typed error.
fn finite(value: f64, operation: &str) -> Result<f64, CommandError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(CommandError::new(
            format!("{operation} overflowed the f64 range"),
            "OVERFLOW",
            "Use smaller operands; f64 arithmetic saturates to infinity past ~1.8e308.",
        )
        .exit_code(ExitCode::USAGE))
    }
}

#[tokio::main]
async fn main() {
    let cli = AgentCli::new("calc", "Agent-native calculator")
        .version("1.0.0")
        .command(
            Command::new("add", "Add two numbers")
                .usage("calc add <a> <b>")
                .handler(|req, _ctx| {
                    // Pull typed args out before the `async move` block: the
                    // future captures by move, so borrow the request first.
                    let a = req.arg_parse::<f64>(0, "a");
                    let b = req.arg_parse::<f64>(1, "b");
                    Box::pin(async move {
                        let sum = finite(a? + b?, "add")?;
                        Ok(CommandOutput::new(json!({
                            "operation": "add",
                            "result": sum,
                        }))
                        .next_action(
                            NextAction::new("calc add <a> <b>", "Add two more numbers")
                                .with_param(
                                    "a",
                                    ActionParam::new().value(json!(sum)).description(
                                        "First number (pre-filled with previous result)",
                                    ),
                                )
                                .with_param(
                                    "b",
                                    ActionParam::new()
                                        .required(true)
                                        .description("Second number"),
                                ),
                        )
                        .next_action(
                            NextAction::new("calc sub <a> <b>", "Subtract instead")
                                .with_param("a", ActionParam::new().value(json!(sum)))
                                .with_param("b", ActionParam::new().required(true)),
                        ))
                    })
                }),
        )
        .command(
            Command::new("sub", "Subtract two numbers")
                .usage("calc sub <a> <b>")
                .handler(|req, _ctx| {
                    let a = req.arg_parse::<f64>(0, "a");
                    let b = req.arg_parse::<f64>(1, "b");
                    Box::pin(async move {
                        let diff = finite(a? - b?, "sub")?;
                        Ok(CommandOutput::new(json!({
                            "operation": "sub",
                            "result": diff,
                        }))
                        .next_action(
                            NextAction::new("calc sub <a> <b>", "Subtract two more numbers")
                                .with_param("a", ActionParam::new().value(json!(diff)))
                                .with_param("b", ActionParam::new().required(true)),
                        ))
                    })
                }),
        )
        .skill();

    let run = cli.run_env().await;
    println!("{}", run.to_json());
    std::process::exit(run.exit_code());
}
