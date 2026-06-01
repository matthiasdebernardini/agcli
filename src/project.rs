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
///   (`"a.b"`), which preserves nesting in the output. Missing paths are
///   silently skipped (selection never errors).
/// - **Array**: apply [`select`] to each element — the natural behavior for
///   list results like `[{id, name, body}, …]` projected to `id,name`.
/// - **Anything else**: returned unchanged.
pub fn select(value: &Value, fields: &[&str]) -> Value {
    match value {
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| select(item, fields)).collect())
        }
        Value::Object(map) => {
            let mut out = Map::new();
            for field in fields {
                let path: Vec<&str> = field.split('.').filter(|s| !s.is_empty()).collect();
                if !path.is_empty() {
                    insert_path(map, &path, &mut out);
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Descend `source` along `path`, inserting the found leaf into `out` while
/// reconstructing intermediate objects so nesting is preserved. An array
/// encountered mid-path is descended element-wise — `checks.fix` over
/// `{ checks: [ {fix}, … ] }` yields `{ checks: [ {fix}, … ] }` — matching the
/// element-wise convention [`select`] already uses for top-level arrays.
fn insert_path(source: &Map<String, Value>, path: &[&str], out: &mut Map<String, Value>) {
    let (head, rest) = path.split_first().expect("path is non-empty");
    let Some(found) = source.get(*head) else {
        return;
    };
    if rest.is_empty() {
        out.insert((*head).to_string(), found.clone());
        return;
    }
    match found {
        Value::Object(inner) => {
            let entry = out
                .entry((*head).to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(target) = entry {
                insert_path(inner, rest, target);
            }
        }
        Value::Array(items) => {
            // Descend the remaining path into each object element. Merge into an
            // existing array target so multiple paths into the same array
            // (`checks.name,checks.fix`) accumulate rather than overwrite.
            let entry = out
                .entry((*head).to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(target) = entry {
                if target.len() < items.len() {
                    target.resize(items.len(), Value::Object(Map::new()));
                }
                for (slot, item) in target.iter_mut().zip(items.iter()) {
                    if let (Value::Object(target_obj), Value::Object(item_obj)) = (slot, item) {
                        insert_path(item_obj, rest, target_obj);
                    }
                }
            }
        }
        // Scalar mid-path: cannot descend. Leave unselected (consistent with a
        // missing path) rather than emitting a partial/empty entry.
        _ => {}
    }
}

/// Recursively drop low-signal fields to shrink agent context.
///
/// Removes `null` values and containers that become empty after
/// compaction. Falsy-but-meaningful values (`false`, `0`, `""`) are kept,
/// and array elements keep their positions (only their contents compact) so
/// indices stay stable.
pub fn compact(value: &Value) -> Value {
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
fn is_droppable(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
