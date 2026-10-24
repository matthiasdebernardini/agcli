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

/// True when `key` can be named in a `--select` value: `--select` splits
/// its value on `,`, trims each field and reads `.` as a path step, so a key
/// holding any of those is one no `--select` can reach.
pub(crate) fn is_selectable_key(key: &str) -> bool {
    !key.contains([',', '.']) && key.trim() == key && !key.is_empty()
}

/// The pre-filled `--select` template offered for a list result: the same
/// invocation re-run projected to a subset of `fields`.
pub(crate) fn advertisement_action(invocation: &Invocation, fields: &[String]) -> NextAction {
    // The tokens are shell-quoted, so the template re-runs exactly this
    // invocation even when an argument contains a space, and the flag sits
    // before any `--`, so the re-run reads it as a flag.
    NextAction::new(
        invocation.command_line_with_flag("--select=<fields>"),
        "Re-run projected to only the fields you need: a smaller result, the same data",
    )
    .with_param(
        "fields",
        ActionParam::new().required(true).description(format!(
            "Comma-separated subset of: {} (dot paths project each row)",
            fields.join(", ")
        )),
    )
}

/// Apply a `--select` value to a result, without ever wiping it.
///
/// `raw` is the flag's value, or the empty string when it was given bare.
/// Three things can go wrong, and each keeps the agent's data and tells it
/// how to correct the select instead of answering `ok: true` with an empty
/// `result`: the value names no field, no field matches anything, or the
/// result has nothing a `--select` can project. A field that matches nothing
/// beside fields that do is dropped from the projection and named. An error
/// envelope is deliberately avoided: the handler already ran, possibly with
/// side effects, so signalling failure could trigger an unwanted retry.
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
    let Some((kind, available)) = selectable_fields(&result) else {
        return (
            result,
            Some(format!(
                "--select={} cannot project this result: it is not an object or an array \
                 of objects. Returning the full result.",
                fields.join(",")
            )),
        );
    };
    let available = describe_available(kind, &available);
    let missed: Vec<&str> = fields
        .iter()
        .copied()
        .filter(|field| !matches_anything(&result, field))
        .collect();
    if missed.len() == fields.len() {
        return (
            result,
            Some(format!(
                "--select={} matched no fields. {available} Returning the full result; \
                 re-run --select with a valid field name.",
                fields.join(",")
            )),
        );
    }
    let projected = project::select(&result, &fields);
    if missed.is_empty() {
        return (projected, None);
    }
    // A partial miss: without this, a typo'd field in a multi-field select
    // is silently dropped and the agent never learns the spelling.
    (
        projected,
        Some(format!(
            "--select field(s) matched nothing and were dropped: {}. {available}",
            missed.join(", ")
        )),
    )
}

/// What a `--select` on `result` can name: the kind of field (`top-level`
/// for an object, `row` for an array) and the sorted fields, an object's keys
/// or the union of the rows' keys. `None` when the result is neither an object
/// nor an array with an object row, so no select can project it.
fn selectable_fields(result: &Value) -> Option<(&'static str, Vec<String>)> {
    match result {
        Value::Object(map) => Some(("top-level", map.keys().cloned().collect())),
        Value::Array(rows) => {
            let objects: Vec<&Map<String, Value>> =
                rows.iter().filter_map(Value::as_object).collect();
            if objects.is_empty() {
                return None;
            }
            let keys: BTreeSet<&String> = objects.iter().flat_map(|row| row.keys()).collect();
            Some(("row", keys.into_iter().cloned().collect()))
        }
        _ => None,
    }
}

/// The sentence of a warning that lists the fields a select could have named.
fn describe_available(kind: &str, available: &[String]) -> String {
    if available.is_empty() {
        format!("The result has no {kind} fields.")
    } else {
        format!("Available {kind} fields: {}.", available.join(", "))
    }
}

/// True when `field`, selected on its own, keeps anything of `result`: a
/// non-empty object, or an array with a non-empty object row.
fn matches_anything(result: &Value, field: &str) -> bool {
    match project::select(result, slice::from_ref(&field)) {
        Value::Object(projected) => !projected.is_empty(),
        Value::Array(rows) => rows
            .iter()
            .any(|row| row.as_object().is_some_and(|row| !row.is_empty())),
        _ => false,
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
        assert_eq!(envelope.result, json!({ "id": 1 }));
        assert_eq!(
            envelope.select_warning.as_deref(),
            Some(
                "--select field(s) matched nothing and were dropped: nonexistnt. Available \
                 top-level fields: body, id, name, note, tags."
            )
        );
    }

    // --- --select projection cluster: never silently wipe a result ---

    #[tokio::test]
    async fn bare_select_keeps_result_and_warns_that_no_field_was_named() {
        // `get 1 --select`: the parser reads a bare flag as `"true"`, but the
        // caller typed no field, so the warning must not blame a `--select=true`
        // nobody wrote.
        let cli = rich_cli();
        for argv in [
            &["app", "get", "1", "--select"][..],
            &["app", "get", "1", "--select", "--quiet"],
            &["app", "get", "1", "--select="],
        ] {
            let run = cli.run(argv.iter().copied()).await;
            let Envelope::Success(envelope) = run.envelope() else {
                panic!("expected success envelope");
            };
            assert_eq!(envelope.result["id"], json!(1), "{argv:?}");
            assert_eq!(
                envelope.select_warning.as_deref(),
                Some(
                    "--select was given no field names. Returning the full result. Re-run \
                     with --select=<field>[,<field>...]."
                ),
                "{argv:?}"
            );
        }
        // A field literally spelled `true` is a field, and it is missing.
        let run = cli.run(["app", "get", "1", "--select=true"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(
            envelope
                .select_warning
                .as_deref()
                .is_some_and(|warning| warning.starts_with("--select=true matched no fields.")),
            "{:?}",
            envelope.select_warning
        );
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

    #[test]
    fn select_partial_miss_on_an_array_warns_like_an_object() {
        // `naem` matches no row: the projection keeps `id`, and the warning
        // names the miss. It used to be dropped without a word.
        let rows = json!([{ "id": 1, "name": "a" }, { "id": 2, "name": "b" }]);
        let (result, warning) = apply_select_flag(rows, "id,naem");
        assert_eq!(result, json!([{ "id": 1 }, { "id": 2 }]));
        assert_eq!(
            warning.as_deref(),
            Some(
                "--select field(s) matched nothing and were dropped: naem. Available row \
                 fields: id, name."
            )
        );
    }

    #[test]
    fn a_dot_path_that_matches_nothing_warns_like_a_top_level_miss() {
        // `a.zzz` used to project to `{ a: {} }`, an empty container that
        // passed for a hit: `ok: true`, no warning, and the data gone.
        let result = json!({ "a": { "b": 1 }, "d": 3 });
        let (kept, warning) = apply_select_flag(result.clone(), "a.zzz");
        assert_eq!(kept, result);
        assert_eq!(
            warning.as_deref(),
            Some(
                "--select=a.zzz matched no fields. Available top-level fields: a, d. \
                 Returning the full result; re-run --select with a valid field name."
            )
        );
        let (kept, warning) = apply_select_flag(result, "a.zzz,d");
        assert_eq!(kept, json!({ "d": 3 }));
        assert_eq!(
            warning.as_deref(),
            Some(
                "--select field(s) matched nothing and were dropped: a.zzz. Available \
                 top-level fields: a, d."
            )
        );
    }

    #[tokio::test]
    async fn a_dot_path_no_row_of_a_list_has_warns() {
        // The advertised `items.<key>` shape, with a key no row has.
        let cli =
            AgentCli::new("app", "x").command(Command::new("ls", "ls").usage("app ls").handler(
                |_req| async move {
                    Ok(CommandOutput::list(vec![
                        json!({ "id": 1, "name": "a" }),
                        json!({ "id": 2, "name": "b" }),
                    ]))
                },
            ));
        let run = cli.run(["app", "ls", "--select=items.zzz"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(
            envelope.result["count"],
            json!(2),
            "the full result is kept"
        );
        assert!(
            envelope
                .select_warning
                .as_deref()
                .is_some_and(|warning| warning.starts_with("--select=items.zzz matched no fields.")),
            "{:?}",
            envelope.select_warning
        );

        let run = cli.run(["app", "ls", "--select=items.id,items.zzz"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(
            envelope.result,
            json!({ "items": [{ "id": 1 }, { "id": 2 }] })
        );
        assert_eq!(
            envelope.select_warning.as_deref(),
            Some(
                "--select field(s) matched nothing and were dropped: items.zzz. Available \
                 top-level fields: count, fields, items, total, truncated."
            )
        );
    }

    #[test]
    fn a_result_nothing_can_project_is_kept_with_a_warning() {
        for result in [json!(42), json!("text"), json!([1, 2, 3]), json!(null)] {
            let (kept, warning) = apply_select_flag(result.clone(), "id");
            assert_eq!(kept, result);
            assert_eq!(
                warning.as_deref(),
                Some(
                    "--select=id cannot project this result: it is not an object or an array \
                     of objects. Returning the full result."
                ),
                "{result}"
            );
        }
        let (kept, warning) = apply_select_flag(json!({}), "id");
        assert_eq!(kept, json!({}));
        assert_eq!(
            warning.as_deref(),
            Some(
                "--select=id matched no fields. The result has no top-level fields. Returning \
                 the full result; re-run --select with a valid field name."
            )
        );
    }

    #[test]
    fn selectable_keys_are_the_ones_a_select_value_can_spell() {
        for key in ["id", "user_name", "a-b", "with space", "ünï"] {
            assert!(is_selectable_key(key), "{key:?}");
        }
        for key in ["a.b", "a,b", " a", "a ", ""] {
            assert!(!is_selectable_key(key), "{key:?}");
        }
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

    #[tokio::test]
    async fn the_advertisement_puts_the_flag_before_the_separator() {
        // Appended after `--`, the filled template would parse the flag as a
        // positional, and the advertisement would grow on every hop.
        let cli = AgentCli::new("app", "x").command(
            Command::new("ls", "ls")
                .usage("app ls [args...]")
                .handler(|_req| async move { Ok(CommandOutput::list(vec![json!({ "id": 1 })])) }),
        );
        let run = cli.run(["app", "ls", "--", "-x"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        let advertised = envelope
            .next_actions
            .iter()
            .find(|action| action.command.contains("--select=<fields>"))
            .expect("the advertisement");
        assert_eq!(advertised.command, "app ls --select=<fields> -- -x");

        // Filled in, it projects, and advertises nothing further.
        let run = cli
            .run(["app", "ls", "--select=items.id", "--", "-x"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result, json!({ "items": [{ "id": 1 }] }));
        assert!(envelope.select_warning.is_none());
        assert!(
            envelope
                .next_actions
                .iter()
                .all(|action| !action.command.contains("--select=<fields>"))
        );
    }
}
