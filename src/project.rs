//! Result projection helpers for token-economy output.
//!
//! These power the framework-reserved `--select` and `--compact` flags,
//! applied centrally so every command inherits them. Kept deliberately
//! simple and predictable: top-level keys plus single-`.`-separated dot
//! paths, mapped element-wise over arrays. Wildcards and array indexing are
//! intentionally out of scope.

use serde_json::{Map, Value};

/// Project `value` to only the named fields.
///
/// - **Object**: keep only `fields`. Each field may be a dot path
///   (`"a.b"`), which preserves nesting in the output. A path that matches
///   nothing adds nothing, not even an empty container: `a.zzz` over
///   `{ a: { b: 1 } }` is `{}`, so a caller can tell a miss from a hit.
/// - **Array**: apply [`select`] to each element, the natural behavior for
///   list results like `[{id, name, body}, ...]` projected to `id,name`.
/// - **Anything else**: returned unchanged.
pub(crate) fn select(value: &Value, fields: &[&str]) -> Value {
    match value {
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| select(item, fields)).collect())
        }
        Value::Object(map) => {
            let mut out = Map::new();
            for field in fields {
                let path: Vec<&str> = field.split('.').filter(|s| !s.is_empty()).collect();
                if let Some(projected) = project_path(map, &path) {
                    merge(&mut out, projected);
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// `source` reduced to the leaf `path` names, with the objects on the way
/// rebuilt around it, or `None` when nothing on the path exists. An array met
/// mid-path is descended element-wise: `checks.fix` over
/// `{ checks: [ {fix}, ... ] }` yields `{ checks: [ {fix}, ... ] }`, the
/// convention [`select`] uses for top-level arrays. Rows without the leaf stay
/// as `{}` so positions hold, and when no row has it the path is a miss.
fn project_path(source: &Map<String, Value>, path: &[&str]) -> Option<Map<String, Value>> {
    let (head, rest) = path.split_first()?;
    let found = source.get(*head)?;
    let projected = if rest.is_empty() {
        found.clone()
    } else {
        match found {
            Value::Object(inner) => Value::Object(project_path(inner, rest)?),
            Value::Array(items) => {
                let rows: Vec<Option<Map<String, Value>>> = items
                    .iter()
                    .map(|item| item.as_object().and_then(|row| project_path(row, rest)))
                    .collect();
                if rows.iter().all(Option::is_none) {
                    return None;
                }
                // A row without the leaf stays `{}`, so positions hold.
                Value::Array(
                    rows.into_iter()
                        .map(|row| row.map_or_else(|| Value::Object(Map::new()), Value::Object))
                        .collect(),
                )
            }
            // A scalar mid-path cannot be descended: the path names nothing.
            _ => return None,
        }
    };
    let mut out = Map::new();
    out.insert((*head).to_string(), projected);
    Some(out)
}

/// Merge `incoming` into `target`, so several paths into one object or array
/// (`checks.name,checks.fix`) accumulate instead of the last one winning.
/// Objects merge key by key and arrays row by row; anything else is replaced.
fn merge(target: &mut Map<String, Value>, incoming: Map<String, Value>) {
    for (key, value) in incoming {
        match (target.get_mut(&key), value) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => merge(existing, incoming),
            (Some(Value::Array(existing)), Value::Array(incoming)) => {
                for (slot, row) in existing.iter_mut().zip(incoming) {
                    if let (Value::Object(slot), Value::Object(row)) = (slot, row) {
                        merge(slot, row);
                    }
                }
            }
            (_, value) => {
                target.insert(key, value);
            }
        }
    }
}

/// Recursively drop low-signal fields to shrink agent context.
///
/// Removes `null` values and containers that become empty after
/// compaction. Falsy-but-meaningful values (`false`, `0`, `""`) are kept,
/// and array elements keep their positions (only their contents compact) so
/// indices stay stable.
pub(crate) fn compact(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, val) in map {
                let compacted = compact(val);
                if is_droppable(&compacted) {
                    continue;
                }
                out.insert(key.clone(), compacted);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(compact).collect()),
        other => other.clone(),
    }
}

/// True for values that carry no signal once compacted: `null` and empty
/// containers. Empty strings and falsy scalars are retained on purpose.
pub(crate) fn is_droppable(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// True for values with nothing but droppable leaves: a droppable value, or
/// an object or array whose every member is hollow. `--compact` keeps the
/// rows of an array, so a selection it empties can still be `[{}, {}]`.
pub(crate) fn is_hollow(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.values().all(is_hollow),
        Value::Array(items) => items.iter().all(is_hollow),
        other => is_droppable(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_hollow_sees_only_droppable_leaves() {
        assert!(is_hollow(&json!(null)));
        assert!(is_hollow(&json!([])));
        assert!(is_hollow(&json!({})));
        assert!(is_hollow(&json!({ "a": [{}, { "b": null }], "c": [] })));
        assert!(!is_hollow(&json!({ "a": [{}, { "b": 0 }] })));
        assert!(!is_hollow(&json!(false)));
        // An empty string is kept by `--compact`, so it is not hollow.
        assert!(!is_hollow(&json!("")));
        assert!(!is_hollow(&json!({ "a": [""] })));
    }

    #[test]
    fn select_keeps_only_named_top_level_keys() {
        let value = json!({ "id": 1, "name": "x", "body": "long…" });
        let out = select(&value, &["id", "name"]);
        assert_eq!(out, json!({ "id": 1, "name": "x" }));
    }

    #[test]
    fn select_missing_key_is_noop() {
        let value = json!({ "id": 1 });
        let out = select(&value, &["id", "missing"]);
        assert_eq!(out, json!({ "id": 1 }));
    }

    #[test]
    fn select_dot_path_preserves_nesting() {
        let value = json!({ "a": { "b": 2, "c": 3 }, "d": 4 });
        let out = select(&value, &["a.b"]);
        assert_eq!(out, json!({ "a": { "b": 2 } }));
    }

    #[test]
    fn select_maps_over_array_elements() {
        let value = json!([
            { "id": 1, "name": "a", "extra": true },
            { "id": 2, "name": "b", "extra": false }
        ]);
        let out = select(&value, &["id", "name"]);
        assert_eq!(
            out,
            json!([{ "id": 1, "name": "a" }, { "id": 2, "name": "b" }])
        );
    }

    #[test]
    fn select_on_scalar_returns_unchanged() {
        assert_eq!(select(&json!(42), &["id"]), json!(42));
    }

    #[test]
    fn select_dot_path_descends_into_array_elementwise() {
        let value = json!({
            "healthy": false,
            "checks": [
                { "name": "auth", "ok": false, "fix": "Set API_TOKEN" },
                { "name": "ping", "ok": true }
            ]
        });
        let out = select(&value, &["checks.fix"]);
        assert_eq!(out, json!({ "checks": [ { "fix": "Set API_TOKEN" }, {} ] }));
    }

    #[test]
    fn select_multiple_dot_paths_into_same_array_merge() {
        let value = json!({
            "checks": [
                { "name": "auth", "ok": false, "fix": "Set API_TOKEN" }
            ]
        });
        let out = select(&value, &["checks.name", "checks.fix"]);
        assert_eq!(
            out,
            json!({ "checks": [ { "name": "auth", "fix": "Set API_TOKEN" } ] })
        );
    }

    #[test]
    fn select_multiple_dot_paths_into_same_object_merge() {
        let value = json!({ "a": { "b": { "c": 1, "d": 2, "e": 3 } } });
        let out = select(&value, &["a.b.c", "a.b.d"]);
        assert_eq!(out, json!({ "a": { "b": { "c": 1, "d": 2 } } }));
        // A whole key and a path into it agree on the whole key.
        let out = select(&value, &["a.b.c", "a"]);
        assert_eq!(out, value);
    }

    #[test]
    fn a_dot_path_that_matches_nothing_adds_no_container() {
        // Before, `a.zzz` left `{ a: {} }` behind: an empty object that read
        // as a hit, so no `select_warning` was raised.
        let value = json!({ "a": { "b": 1 }, "d": 3 });
        assert_eq!(select(&value, &["a.zzz"]), json!({}));
        assert_eq!(select(&value, &["a.b.c"]), json!({}), "a scalar mid-path");
        assert_eq!(select(&value, &["zzz.b"]), json!({}));
        // One miss beside a hit leaves only the hit.
        assert_eq!(select(&value, &["a.zzz", "d"]), json!({ "d": 3 }));
    }

    #[test]
    fn a_dot_path_no_row_has_adds_no_array() {
        let value = json!({ "items": [{ "id": 1 }, { "id": 2 }], "count": 2 });
        assert_eq!(select(&value, &["items.zzz"]), json!({}));
        // One row with the leaf keeps every row, so positions hold.
        let value = json!({ "items": [{ "id": 1 }, { "tag": "t" }] });
        assert_eq!(
            select(&value, &["items.tag"]),
            json!({ "items": [{}, { "tag": "t" }] })
        );
    }

    #[test]
    fn compact_drops_null_and_empty_containers() {
        let value = json!({
            "id": 1,
            "note": null,
            "tags": [],
            "meta": {},
            "kept_false": false,
            "kept_zero": 0,
            "kept_empty_str": ""
        });
        let out = compact(&value);
        assert_eq!(
            out,
            json!({ "id": 1, "kept_false": false, "kept_zero": 0, "kept_empty_str": "" })
        );
    }

    #[test]
    fn compact_recurses_and_drops_emptied_objects() {
        let value = json!({ "outer": { "inner": null } });
        let out = compact(&value);
        // inner -> null dropped, outer becomes {} and is dropped too.
        assert_eq!(out, json!({}));
    }

    #[test]
    fn compact_keeps_array_positions() {
        let value = json!({ "xs": [{ "a": null, "b": 1 }, { "a": 2 }] });
        let out = compact(&value);
        assert_eq!(out, json!({ "xs": [{ "b": 1 }, { "a": 2 }] }));
    }
}
