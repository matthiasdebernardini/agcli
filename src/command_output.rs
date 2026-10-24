//! [`CommandOutput`]: the success payload a handler returns.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{Value, json};

use crate::codes::ExitCode;
use crate::next_action::NextAction;
use crate::select;

/// Derive a list's row schema as sorted `--select` paths, ready to paste into
/// `--select` verbatim.
///
/// Each distinct top-level key of the object items becomes the dot path
/// `items.<key>`, because `--select` projects *top-level* result keys: a bare
/// `id` would miss the rows nested under `items` and dead-end in a
/// `select_warning`. Rows are commonly heterogeneous (optional fields, mixed
/// variants), so the union across items beats sampling the first row. Keys go
/// through a [`BTreeSet`], so the paths are deduplicated and sorted: the order
/// does not depend on `serde_json`'s map backing, which a `preserve_order`
/// feature unification elsewhere in the dependency graph could otherwise flip.
/// Non-object items contribute nothing, so a list of scalars yields an empty
/// schema, and a key no `--select` value can spell (one holding `.` or `,`)
/// is left out rather than advertised as a path that cannot work.
fn row_schema(items: &[Value]) -> Vec<String> {
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    for item in items {
        let Value::Object(map) = item else { continue };
        keys.extend(
            map.keys()
                .map(String::as_str)
                .filter(|key| select::is_selectable_key(key)),
        );
    }
    keys.into_iter().map(|key| format!("items.{key}")).collect()
}

/// What a handler returns on success: the `result` payload, the
/// `next_actions` that follow from it, and an optional exit code.
///
/// The framework turns it into the success envelope, applying `--select`,
/// `--compact` and `--quiet` on the way. Build one with
/// [`CommandOutput::new`], or [`CommandOutput::list`] for a bounded list.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandOutput {
    pub(crate) result: Value,
    pub(crate) next_actions: Vec<NextAction>,
    /// Optional process exit-code override. `None` → [`ExitCode::SUCCESS`].
    /// Lets a command report a non-zero status while still emitting an
    /// `ok: true` envelope (e.g. `doctor` reporting an unhealthy system).
    pub(crate) exit_code: Option<ExitCode>,
    /// When `--compact` is requested, project to these fields instead of the
    /// generic null/empty drop. `None` → generic compaction.
    pub(crate) compact_fields: Option<Vec<String>>,
    /// The `--select` paths covering the row schema, as
    /// [`CommandOutput::list_truncated`] derived them from the items: one
    /// sorted `items.<key>` dot path per distinct top-level key across the
    /// rows. The dispatch path uses it to offer a pre-filled `--select`
    /// template `next_action` so an agent can re-run this exact invocation
    /// projected to fewer fields. `None` for every non-list output.
    pub(crate) select_fields: Option<Vec<String>>,
}

impl CommandOutput {
    /// An output whose envelope `result` is `result`, with no `next_actions`
    /// of its own and exit code 0.
    #[must_use]
    pub fn new(result: Value) -> Self {
        Self {
            result,
            next_actions: Vec::new(),
            exit_code: None,
            compact_fields: None,
            select_fields: None,
        }
    }

    /// Build an output whose result is `value` serialized to JSON.
    ///
    /// # Errors
    ///
    /// The [`serde_json::Error`] from [`serde_json::to_value`] if `value`'s
    /// `Serialize` impl fails, or it is a map with non-string keys.
    pub fn try_from_serializable<S>(value: S) -> Result<Self, serde_json::Error>
    where
        S: Serialize,
    {
        Ok(Self::new(serde_json::to_value(value)?))
    }

    /// Build a list result of every row: `{ items, count, total, truncated }`
    /// with `count == total` and `truncated == false`, plus the `fields` row
    /// schema of [`CommandOutput::list_truncated`].
    #[must_use]
    pub fn list(items: Vec<Value>) -> Self {
        let total = items.len();
        Self::list_truncated(items, total)
    }

    /// Build a bounded list result where `total` may exceed the returned
    /// items: `{ items, count, total, truncated }`. When `total` is more than
    /// the items, `truncated` is true and a `guidance` string tells the agent
    /// how to narrow the query.
    ///
    /// The row schema is published as a `fields` key holding the `--select`
    /// paths that cover it: one sorted `items.<key>` dot path per distinct
    /// top-level key across the object rows (`["items.id", "items.name"]`). An
    /// agent so learns the shape of a row, and gets a projection it can paste
    /// into `--select` as it is, without decoding a row. `fields` is schema
    /// disclosure, emitted whether or not the CLI enables the reserved flags,
    /// because the output cannot see that configuration. When they are on, the
    /// framework also appends a `next_action` that reruns this invocation under
    /// `--select`.
    ///
    /// # Panics
    ///
    /// When `total` is less than the number of items: a list cannot hold more
    /// rows than exist, and reporting one as `truncated: false` with a `total`
    /// below its `count` would be a wrong answer.
    #[must_use]
    pub fn list_truncated(items: Vec<Value>, total: usize) -> Self {
        let count = items.len();
        assert!(
            total >= count,
            "a list of {count} items cannot have a total of {total}"
        );
        let truncated = count < total;
        let fields = row_schema(&items);
        let mut result = serde_json::Map::new();
        result.insert("items".to_string(), Value::Array(items));
        result.insert("count".to_string(), json!(count));
        result.insert("total".to_string(), json!(total));
        result.insert("truncated".to_string(), json!(truncated));
        if !fields.is_empty() {
            result.insert("fields".to_string(), json!(fields));
        }
        if truncated {
            result.insert(
                "guidance".to_string(),
                json!(format!(
                    "Showing {count} of {total} results. Narrow the query with the command's \
                     own flags, or project the rows with --select."
                )),
            );
        }
        let mut output = Self::new(Value::Object(result));
        if !fields.is_empty() {
            output.select_fields = Some(fields);
        }
        output
    }

    /// Add a `next_action` that follows from this result, pre-filling what the
    /// agent now knows with [`ActionParam::value`](crate::ActionParam::value).
    /// Without any, the framework offers the command's defaults.
    #[must_use]
    pub fn next_action(mut self, action: NextAction) -> Self {
        self.next_actions.push(action);
        self
    }

    /// Add several `next_actions` at once, in order. See
    /// [`CommandOutput::next_action`].
    #[must_use]
    pub fn next_actions(mut self, actions: impl IntoIterator<Item = NextAction>) -> Self {
        self.next_actions.extend(actions);
        self
    }

    /// Report success with a non-zero process exit code: the command ran and
    /// its answer is valid, but the shell should see a problem, as when
    /// `doctor` finds an unhealthy system. The envelope stays `ok: true`.
    #[must_use]
    pub fn exit_code(mut self, code: ExitCode) -> Self {
        self.exit_code = Some(code);
        self
    }

    /// Declare the high-gravity fields `--compact` should keep for this
    /// output. Without this, `--compact` falls back to dropping null/empty
    /// fields generically.
    #[must_use]
    pub fn compact_fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.compact_fields = Some(fields.into_iter().map(Into::into).collect());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agent_cli::AgentCli;
    use crate::command::{Builtin, Command};
    use crate::envelope::{Envelope, SuccessEnvelope};

    #[tokio::test]
    async fn compact_honors_high_gravity_allowlist() {
        let cli = AgentCli::new("app", "x").command(Command::new("get", "get").handler(
            |_req| async move {
                Ok(
                    CommandOutput::new(json!({ "id": 1, "name": "x", "body": "huge" }))
                        .compact_fields(["id", "name"]),
                )
            },
        ));
        let run = cli.run(["app", "get", "--compact"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result, json!({ "id": 1, "name": "x" }));
    }

    // --- Bounded list output ---

    #[tokio::test]
    async fn list_truncated_emits_bounded_envelope() {
        let cli = AgentCli::new("app", "x").command(Command::new("ls", "list").handler(
            |_req| async move { Ok(CommandOutput::list_truncated(vec![json!({ "id": 1 })], 50)) },
        ));
        let run = cli.run(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["count"], json!(1));
        assert_eq!(envelope.result["total"], json!(50));
        assert_eq!(envelope.result["truncated"], json!(true));
        assert!(envelope.result["guidance"].is_string());
    }

    #[tokio::test]
    async fn list_full_has_no_guidance() {
        let items = vec![json!({ "id": 1 }), json!({ "id": 2 })];
        let cli =
            AgentCli::new("app", "x").command(Command::new("ls", "list").handler(move |_req| {
                let items = items.clone();
                async move { Ok(CommandOutput::list(items)) }
            }));
        let run = cli.run(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["truncated"], json!(false));
        assert!(envelope.result.get("guidance").is_none());
    }

    // --- List row schema + --select advertisement ---

    /// A CLI whose single `ls` command returns `items` as a bounded list,
    /// optionally with a handler-supplied `next_action`.
    fn list_cli(items: Vec<Value>, handler_action: bool) -> AgentCli {
        AgentCli::new("app", "x").command(Command::new("ls", "list").usage("app ls").handler(
            move |_req| {
                let items = items.clone();
                async move {
                    let output = CommandOutput::list(items);
                    Ok(if handler_action {
                        output.next_action(NextAction::new("app ls", "List again"))
                    } else {
                        output
                    })
                }
            },
        ))
    }

    fn select_advertisement(envelope: &SuccessEnvelope) -> Option<&NextAction> {
        envelope
            .next_actions
            .iter()
            .find(|a| a.command.contains("--select=<fields>"))
    }

    #[tokio::test]
    async fn list_fields_are_sorted_deduped_select_paths() {
        // Keys chosen so sorted order differs from first-seen order: this
        // pins determinism, not the order the rows happened to be built in.
        let items = vec![
            json!({ "zebra": 1, "alpha": "a" }),
            json!({ "beta": true, "zebra": 2 }),
        ];
        let run = list_cli(items, false).run(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(
            envelope.result["fields"],
            json!(["items.alpha", "items.beta", "items.zebra"])
        );
    }

    /// The contract the whole feature rests on: the advertised paths, fed back
    /// verbatim, project instead of dead-ending in a `select_warning`.
    #[tokio::test]
    async fn advertised_fields_round_trip_into_a_working_select() {
        let items = vec![
            json!({ "id": 1, "name": "a", "body": "long…" }),
            json!({ "id": 2, "name": "b", "body": "longer…" }),
        ];
        let run = list_cli(items.clone(), false).run(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        let fields: Vec<String> = envelope.result["fields"]
            .as_array()
            .expect("fields is an array")
            .iter()
            .map(|field| field.as_str().expect("field is a string").to_string())
            .collect();
        assert_eq!(fields, ["items.body", "items.id", "items.name"]);

        // Re-run with exactly what the envelope advertised.
        let flag = format!("--select={}", fields.join(","));
        let run = list_cli(items.clone(), false)
            .run(["app", "ls", flag.as_str()])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(
            envelope.select_warning.is_none(),
            "advertised paths must project, not warn: {:?}",
            envelope.select_warning
        );
        assert_eq!(envelope.result["items"], json!(items));
        for meta in ["count", "total", "truncated", "fields"] {
            assert!(
                envelope.result.get(meta).is_none(),
                "projection should have dropped the `{meta}` metadata key"
            );
        }

        // A subset projects each row down to just those keys.
        let run = list_cli(items, false)
            .run(["app", "ls", "--select=items.id,items.name"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.select_warning.is_none());
        assert_eq!(
            envelope.result["items"],
            json!([{ "id": 1, "name": "a" }, { "id": 2, "name": "b" }])
        );
    }

    #[test]
    #[should_panic(expected = "a list of 3 items cannot have a total of 1")]
    fn a_total_below_the_item_count_panics() {
        // Reported as it stood, this was `count: 3, total: 1, truncated: false`.
        let _ = CommandOutput::list_truncated(vec![json!(1), json!(2), json!(3)], 1);
    }

    #[tokio::test]
    async fn fields_leave_out_keys_no_select_can_spell() {
        // `--select` splits on `,` and reads `.` as a path step, so `a.b` and
        // `x,y` cannot be named: advertising them would promise a projection
        // that dead-ends in a `select_warning`.
        let items = vec![json!({ "id": 1, "a.b": 2, "x,y": 3, " padded": 4 })];
        let run = list_cli(items, false).run(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["fields"], json!(["items.id"]));
    }

    #[tokio::test]
    async fn compact_beside_select_keeps_the_selection() {
        // `--select=body --compact` used to re-project the selection to the
        // command's `compact_fields` and answer `result: {}` without a word.
        // An explicit --select wins; --compact then only drops nulls.
        let cli = AgentCli::new("app", "x").command(Command::new("get", "get").handler(
            |_req| async move {
                Ok(CommandOutput::new(
                    json!({ "id": 1, "name": "x", "body": "huge", "note": null }),
                )
                .compact_fields(["id", "name"]))
            },
        ));
        let run = cli
            .run(["app", "get", "--select=body,note", "--compact"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result, json!({ "body": "huge" }));
        assert!(envelope.select_warning.is_none());
    }

    #[tokio::test]
    async fn list_of_non_objects_has_no_fields_or_select_action() {
        let items = vec![json!("alpha"), json!("beta")];
        let run = list_cli(items, false).run(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(envelope.result.get("fields").is_none());
        assert!(select_advertisement(envelope).is_none());
    }

    #[tokio::test]
    async fn empty_list_has_no_fields_or_select_action() {
        let run = list_cli(Vec::new(), false).run(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["count"], json!(0));
        assert!(envelope.result.get("fields").is_none());
        assert!(select_advertisement(envelope).is_none());
    }

    #[tokio::test]
    async fn list_envelope_advertises_prefilled_select_template() {
        let items = vec![json!({ "id": 1, "name": "a" })];
        let run = list_cli(items, false).run(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        let action = select_advertisement(envelope).expect("--select advertisement present");
        // The template re-runs *this* invocation, verbatim, plus the flag.
        assert_eq!(action.command, "app ls --select=<fields>");
        let param = action
            .params
            .as_ref()
            .expect("params make the command a template")
            .get("fields")
            .expect("fields param");
        assert_eq!(param.required, Some(true));
        let description = param.description.as_deref().expect("fields description");
        assert!(
            description.contains("items.id") && description.contains("items.name"),
            "description names the working --select paths: {description}"
        );
    }

    #[tokio::test]
    async fn advertised_command_shell_quotes_argv_tokens() {
        let items = vec![json!({ "id": 1 })];
        let cli = AgentCli::new("app", "x").command(
            Command::new("ls", "list")
                .usage("app ls [--filter <value>]")
                .handler(move |_req| {
                    let items = items.clone();
                    async move { Ok(CommandOutput::list(items)) }
                }),
        );
        let run = cli.run(["app", "ls", "--filter", "two words"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        let action = select_advertisement(envelope).expect("--select advertisement present");
        assert_eq!(
            action.command, "app ls --filter 'two words' --select=<fields>",
            "an advertised command must be runnable as printed"
        );
    }

    #[tokio::test]
    async fn select_advertisement_appends_to_handler_next_actions() {
        let items = vec![json!({ "id": 1 })];
        let run = list_cli(items, true).run(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.next_actions.len(), 2);
        assert_eq!(envelope.next_actions[0].command, "app ls");
        assert_eq!(envelope.next_actions[0].description, "List again");
        assert!(select_advertisement(envelope).is_some());
    }

    #[tokio::test]
    async fn select_flag_suppresses_the_advertisement_and_still_projects() {
        let items = vec![json!({ "id": 1, "name": "a" })];
        let run = list_cli(items, false)
            .run(["app", "ls", "--select=items.id"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(
            select_advertisement(envelope).is_none(),
            "no re-advertisement once --select is in use"
        );
        // Projection still works exactly as before: only the selected path.
        assert_eq!(envelope.result["items"], json!([{ "id": 1 }]));
        assert!(envelope.result.get("fields").is_none());
        assert!(envelope.result.get("count").is_none());
    }

    #[tokio::test]
    async fn truncated_list_carries_both_guidance_and_fields() {
        let cli =
            AgentCli::new("app", "x").command(Command::new("ls", "list").usage("app ls").handler(
                |_req| async move {
                    Ok(CommandOutput::list_truncated(
                        vec![json!({ "id": 1, "name": "a" })],
                        9,
                    ))
                },
            ));
        let run = cli.run(["app", "ls"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["truncated"], json!(true));
        assert!(
            envelope.result["guidance"]
                .as_str()
                .expect("guidance on a truncated list")
                .contains("1 of 9")
        );
        assert_eq!(
            envelope.result["fields"],
            json!(["items.id", "items.name"]),
            "narrowing guidance and the row schema are complementary, not exclusive"
        );
        assert!(select_advertisement(envelope).is_some());
    }

    #[tokio::test]
    async fn reserved_flags_off_keeps_fields_but_drops_the_advertisement() {
        let items = vec![json!({ "id": 1, "name": "a" })];
        let run = list_cli(items, false)
            .without_reserved_flags()
            .run(["app", "ls"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        // `fields` is schema disclosure the output emits unconditionally; only
        // the advertisement depends on --select actually being available.
        assert_eq!(envelope.result["fields"], json!(["items.id", "items.name"]));
        assert!(select_advertisement(envelope).is_none());
    }

    #[tokio::test]
    async fn reserved_projection_opt_out_drops_the_advertisement() {
        let items = vec![json!({ "id": 1, "name": "a" })];
        let mut command = Command::new("ls", "list")
            .usage("app ls")
            .handler(move |_req| {
                let items = items.clone();
                async move { Ok(CommandOutput::list(items)) }
            });
        // The opt-out `AgentCli::doctor` uses: --select never applies here, so
        // advertising it would send the agent to a flag that does nothing.
        command.builtin = Some(Builtin::Doctor);
        let run = AgentCli::new("app", "x")
            .command(command)
            .run(["app", "ls"])
            .await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert_eq!(envelope.result["fields"], json!(["items.id", "items.name"]));
        assert!(select_advertisement(envelope).is_none());
    }

    #[tokio::test]
    async fn quiet_drops_the_advertisement_with_every_other_next_action() {
        let items = vec![json!({ "id": 1, "name": "a" })];
        let run = list_cli(items, true).run(["app", "ls", "--quiet"]).await;
        let Envelope::Success(envelope) = run.envelope() else {
            panic!("expected success envelope");
        };
        assert!(
            envelope.next_actions.is_empty(),
            "--quiet means no next_actions at all: {:?}",
            envelope.next_actions
        );
        assert_eq!(envelope.result["fields"], json!(["items.id", "items.name"]));
    }
}
