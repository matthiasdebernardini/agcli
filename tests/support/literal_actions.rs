//! A crawl over the literal `next_actions` a CLI answers with, shared by the
//! crate's unit tests and the example tests.
//!
//! The contract: an action without `params` is literal and runs as printed.
//! The crawl runs the root and every command path in the root tree, then every
//! literal action those answers carry, transitively, and fails on any literal
//! the framework rejects as a malformed invocation.

use std::collections::{BTreeSet, VecDeque};

use serde_json::Value;

/// Error codes that mean the argv itself was malformed.
const MALFORMED: [&str; 3] = ["EXTRA_ARG", "UNKNOWN_FLAG", "PARSE_ERROR"];

/// Crawl `program` through `run`, which answers an argv with its envelope as
/// JSON, and return every literal command it ran.
pub async fn literal_actions_run_as_printed(
    program: &str,
    mut run: impl AsyncFnMut(Vec<String>) -> Value,
) -> BTreeSet<String> {
    let root = run(vec![program.to_string()]).await;
    let mut paths = Vec::new();
    command_paths(
        &root["result"]["commands"],
        &mut vec![program.to_string()],
        &mut paths,
    );
    let mut answers = VecDeque::from([root]);
    for path in paths {
        answers.push_back(run(path).await);
    }

    let mut ran = BTreeSet::new();
    while let Some(answer) = answers.pop_front() {
        let actions = answer["next_actions"]
            .as_array()
            .expect("every envelope carries next_actions");
        for action in actions {
            if action.get("params").is_some() {
                continue;
            }
            let command = action["command"].as_str().expect("a command string");
            if !ran.insert(command.to_string()) {
                continue;
            }
            let answer = run(command.split_whitespace().map(str::to_string).collect()).await;
            let code = answer["error"]["code"].as_str();
            assert!(
                code.is_none_or(|code| !MALFORMED.contains(&code)),
                "`{command}` does not run as printed: {answer}"
            );
            answers.push_back(answer);
        }
    }
    ran
}

/// Push the argv of every command in a tree's `commands` array, with its
/// subcommands, prefixed by `prefix`.
fn command_paths(commands: &Value, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    for command in commands.as_array().expect("a commands array") {
        prefix.push(
            command["name"]
                .as_str()
                .expect("a command name")
                .to_string(),
        );
        out.push(prefix.clone());
        if let Some(subcommands) = command.get("subcommands") {
            command_paths(subcommands, prefix, out);
        }
        prefix.pop();
    }
}
