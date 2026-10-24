// The getting-started calculator from the README: `calc add <a> <b>` and
// `calc sub <a> <b>`.
//
// CI builds every example, so this one cannot drift from the API, and the
// README's copy of `cli()` runs as a doctest.
//
// ```bash
// cargo run --example calc                 # the command tree
// cargo run --example calc -- add 3 5      # a result
// cargo run --example calc -- sub 10 4
// cargo run --example calc -- add foo bar  # a typed error, exit code 2
// cargo run --example calc -- skill        # the CLI as a SKILL.md
// ```

use agcli::{
    ActionParam, AgentCli, Command, CommandError, CommandOutput, ErrorCode, ExitCode, NextAction,
};
use serde_json::json;

/// `serde_json` renders non-finite floats as `null`, which would corrupt the
/// result while still reporting `ok: true`, so overflow is a typed error.
fn finite(value: f64, operation: &str) -> Result<f64, CommandError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(CommandError::new(
            ErrorCode::new("OVERFLOW"),
            format!("{operation} overflowed the f64 range"),
            "Use smaller operands; f64 arithmetic saturates to infinity past ~1.8e308.",
        )
        .exit_code(ExitCode::USAGE))
    }
}

/// The calculator's command tree.
fn cli() -> AgentCli {
    AgentCli::new("calc", "Agent-native calculator")
        .version("1.0.0")
        .command(
            Command::new("add", "Add two numbers")
                .usage("calc add <a> <b>")
                .handler(|req| async move {
                    // The request is owned: the typed helpers turn a missing or
                    // unparseable argument into a MISSING_ARG / INVALID_ARG error.
                    let a: f64 = req.arg_parse(0, "a")?;
                    let b: f64 = req.arg_parse(1, "b")?;
                    let sum = finite(a + b, "add")?;
                    Ok(CommandOutput::new(json!({
                        "operation": "add",
                        "result": sum,
                    }))
                    .next_action(
                        NextAction::new("calc add <a> <b>", "Add two more numbers")
                            .with_param(
                                "a",
                                ActionParam::new()
                                    .value(json!(sum))
                                    .description("First number (pre-filled with previous result)"),
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
                }),
        )
        .command(
            Command::new("sub", "Subtract two numbers")
                .usage("calc sub <a> <b>")
                .handler(|req| async move {
                    let a: f64 = req.arg_parse(0, "a")?;
                    let b: f64 = req.arg_parse(1, "b")?;
                    let diff = finite(a - b, "sub")?;
                    Ok(CommandOutput::new(json!({
                        "operation": "sub",
                        "result": diff,
                    }))
                    .next_action(
                        NextAction::new("calc sub <a> <b>", "Subtract two more numbers")
                            .with_param("a", ActionParam::new().value(json!(diff)))
                            .with_param("b", ActionParam::new().required(true)),
                    ))
                }),
        )
        .skill()
}

#[tokio::main]
async fn main() {
    cli().run_env().await.finish()
}
