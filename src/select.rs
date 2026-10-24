//! The reserved `--select` projection and the `--select` advertisement for
//! list results.

use std::collections::BTreeSet;
use std::slice;

use serde_json::{Map, Value};

use crate::invocation::Invocation;
use crate::next_action::{ActionParam, NextAction};
use crate::project;

/// Split a `--select` value (`"id,name, body"`) into trimmed, non-empty
/// field names.
pub(crate) fn parse_select_fields(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(',').map(str::trim).filter(|s| !s.is_empty())
}

/// The pre-filled `--select` template offered for a list result: the same
/// invocation re-run projected to a subset of `fields`.
pub(crate) fn advertisement_action(invocation: &Invocation, fields: &[String]) -> NextAction {
    // `command_line()` is shell-quoted, so the template re-runs exactly this
    // invocation even when an argument contains a space.
    NextAction::new(
        format!("{} --select=<fields>", invocation.command_line()),
        "Re-run projected to only the fields you need — smaller result, same data",
    )
    .with_param(
        "fields",
        ActionParam::new().required(true).description(format!(
            "Comma-separated subset of: {} (dot paths project each row)",
            fields.join(", ")
        )),
    )
}

/// Apply a reserved `--select` value to a handler result, guarding against the
/// silent-erasure footgun. A bare `--select` (parsed as the sentinel `"true"`),
/// an empty value, or a typo'd/array-only field name all project a non-empty
/// object down to `{}`. Rather than discard the handler's real output and
/// report a misleading `ok: true` with `result: {}`, this keeps the original
/// result and returns a warning naming the available fields, so the agent
/// keeps its data and learns how to correct the select. (An error envelope is
/// deliberately avoided: the handler already ran — possibly with side effects
/// — so signalling failure could trigger an unwanted retry.)
///
/// The warning is the envelope's own `select_warning` field, never a key
/// inside `result`: the result belongs to the handler, which may well have a
/// `select_warning` key of its own.
pub(crate) fn apply_select_flag(result: Value, raw: &str) -> (Value, Option<String>) {
    let fields: Vec<&str> = parse_select_fields(raw).collect();
    if fields.is_empty() {
        return (
            result,
            Some(
                "--select was given no field names. Returning the full result. \
                 Re-run with --select=<field>[,<field>...]."
                    .to_string(),
            ),
        );
    }
    let projected = project::select(&result, &fields);
    if let Some((kind, available)) = collapsed(&result, &projected) {
        return (
            result,
            Some(format!(
                "--select={} matched no fields. Available {kind} fields: {}. \
                 Returning the full result; re-run --select with a valid field name.",
                fields.join(","),
                available.join(", ")
            )),
        );
    }
    // Partial miss: some fields matched, others didn't. Without this, a typo'd
    // field in a multi-field select is silently dropped and the agent never
    // learns the spelling. Keep the (useful) projection, but name the misses.
    if let Value::Object(map) = &result {
        let missed: Vec<&&str> = fields
            .iter()
            .filter(|field| {
                matches!(project::select(&result, slice::from_ref(*field)),
                         Value::Object(m) if m.is_empty())
            })
            .collect();
        if !missed.is_empty() {
            let available = map.keys().cloned().collect::<Vec<_>>().join(", ");
            let missed_list = missed
                .iter()
                .map(|s| (**s).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return (
                projected,
                Some(format!(
                    "--select field(s) matched nothing and were dropped: {missed_list}. \
                     Available top-level fields: {available}."
                )),
            );
        }
    }
    (projected, None)
}

/// When `projected` wiped a non-empty `result` — an object down to `{}`, or
/// an array whose object rows all project to `{}` — the kind of field and the
/// sorted fields the select could have named: an object's keys, or the union
/// of the rows' keys.
fn collapsed(result: &Value, projected: &Value) -> Option<(&'static str, Vec<String>)> {
    match (result, projected) {
        (Value::Object(map), Value::Object(out)) if !map.is_empty() && out.is_empty() => {
            Some(("top-level", map.keys().cloned().collect()))
        }
        (Value::Array(rows), Value::Array(out)) => {
            let keys: BTreeSet<&String> = rows
                .iter()
                .filter_map(Value::as_object)
                .flat_map(Map::keys)
                .collect();
            let matched = out
                .iter()
                .any(|row| row.as_object().is_some_and(|row| !row.is_empty()));
            (!keys.is_empty() && !matched).then(|| ("row", keys.into_iter().cloned().collect()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::agent_cli::AgentCli;
    use crate::command::Command;
    use crate::command_output::CommandOutput;
    use crate::envelope::Envelope;
    use crate::test_support::rich_cli;

    // --- Reserved output flag: --select ---

    #[tokio::test]
    async fn select_projects_result_fields() {
        let cli = rich_cli();
        let run = cli.run(["app", "get", "1", "--select", "id,name"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result, json!({ "id": 1, "name": "widget" }));
    }

    // --- select partial-miss warning ---

    #[tokio::test]
    async fn select_partial_miss_warns_about_dropped_fields() {
        let cli = rich_cli();
        let run = cli
            .run(["app", "get", "1", "--select", "id,nonexistnt"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["id"], json!(1));
        let warning = envelope
            .select_warning
            .as_deref()
            .expect("select_warning present");
        assert!(
            warning.contains("nonexistnt"),
            "warning names the miss: {warning}"
        );
    }

    // --- --select projection cluster: never silently wipe a result ---

    #[tokio::test]
    async fn bare_select_keeps_result_and_warns() {
        // `get 1 --select` (bare, flushed to the "true" sentinel) must not wipe
        // the result to {}; it returns the full result plus a select_warning.
        let cli = rich_cli();
        let run = cli.run(["app", "get", "1", "--select"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["id"], json!(1));
        assert!(envelope.select_warning.is_some());
    }

    #[tokio::test]
    async fn typod_select_keeps_result_and_lists_fields() {
        let cli = rich_cli();
        let run = cli.run(["app", "get", "1", "--select=naem"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["name"], json!("widget"));
        let warning = envelope.select_warning.as_deref().expect("warning string");
        assert!(
            warning.contains("name"),
            "warning should list fields: {warning}"
        );
    }

    #[test]
    fn select_matching_no_row_keeps_the_array_and_warns() {
        // No row has the field: the full array comes back, with the union of
        // the rows' keys, sorted.
        let rows = json!([{ "id": 1, "name": "a" }, { "id": 2, "tag": "b" }, 7]);
        let (result, warning) = apply_select_flag(rows.clone(), "naem");
        assert_eq!(result, rows);
        assert_eq!(
            warning.as_deref(),
            Some(
                "--select=naem matched no fields. Available row fields: id, name, tag. \
                 Returning the full result; re-run --select with a valid field name."
            )
        );
    }

    #[test]
    fn select_matching_some_rows_projects_the_others_to_empty() {
        let rows = json!([{ "id": 1, "tag": "a" }, { "id": 2 }]);
        let (result, warning) = apply_select_flag(rows, "tag");
        assert_eq!(result, json!([{ "tag": "a" }, {}]));
        assert_eq!(warning, None);
    }

    #[tokio::test]
    async fn valid_select_has_no_warning() {
        let cli = rich_cli();
        let run = cli.run(["app", "get", "1", "--select=id,name"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result, json!({ "id": 1, "name": "widget" }));
        assert!(envelope.select_warning.is_none());
    }

    #[tokio::test]
    async fn select_warning_never_touches_the_handlers_result() {
        // The handler's own `select_warning` key survives a select that
        // misses: the framework's warning rides at the envelope level.
        let cli =
            AgentCli::new("app", "x").command(Command::new("get", "get").usage("app get").handler(
                |_req| async move {
                    Ok(CommandOutput::new(json!({
                        "id": 1,
                        "select_warning": "the handler's own data",
                    })))
                },
            ));
        let run = cli.run(["app", "get", "--select=nope"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(
            envelope.result["select_warning"],
            json!("the handler's own data")
        );
        assert!(
            envelope
                .select_warning
                .as_deref()
                .is_some_and(|warning| warning.contains("--select=nope matched no fields")),
            "{:?}",
            envelope.select_warning
        );
        let wire: Value = serde_json::from_str(&run.to_json()).expect("JSON");
        assert!(wire["select_warning"].is_string(), "{wire}");
    }
}
