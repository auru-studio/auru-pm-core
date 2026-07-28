//! Recursive 3-way JSON merge for canonical project snapshots.
//!
//! Implements identity-based array merging (elements matched by `"id"` field)
//! with a positional fallback, and standard 3-way scalar/object merging.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

/// A single field that could not be auto-resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct ConflictedField {
    /// Dot-separated path; arrays use `[id=X]` or `.N` notation.
    pub path: String,
    pub ancestor: Option<Value>,
    pub local: Option<Value>,
    pub remote: Option<Value>,
}

/// Result of a 3-way merge.
pub enum MergeOutcome {
    /// Every field was resolved without ambiguity.
    Clean { merged: Value },
    /// At least one field was changed differently by both sides. `base`
    /// has all disjoint changes already applied so the caller only needs
    /// to handle the listed conflicts.
    Conflict {
        base: Value,
        conflicts: Vec<ConflictedField>,
    },
}

/// User choice for one field in a conflicted three-way merge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictChoice {
    Local,
    Remote,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConflictResolution {
    pub conflict: ConflictedField,
    pub choice: ConflictChoice,
}

/// Apply one choice for every conflict to the partially merged value.
///
/// The merge base already contains the local value at conflict locations, so
/// local choices are no-ops. Remote choices replace the value at the stable
/// field path emitted by [`merge3`].
pub fn resolve_conflicts(
    mut base: Value,
    conflicts: &[ConflictedField],
    choices: &[ConflictChoice],
) -> Result<Value, String> {
    if conflicts.len() != choices.len() {
        return Err(format!(
            "expected {} conflict choice(s), received {}",
            conflicts.len(),
            choices.len()
        ));
    }
    for (conflict, choice) in conflicts.iter().zip(choices) {
        if *choice == ConflictChoice::Remote {
            set_conflicted_value(&mut base, &conflict.path, conflict.remote.clone())?;
        }
    }
    Ok(base)
}

#[derive(Debug, Eq, PartialEq)]
enum ConflictPathSegment {
    Key(String),
    Index(usize),
    Id(String),
}

fn conflict_path_segments(path: &str) -> Result<Vec<ConflictPathSegment>, String> {
    let mut segments = Vec::new();
    let mut cursor = 0;
    let bytes = path.as_bytes();
    while cursor < bytes.len() {
        if bytes[cursor] == b'.' {
            cursor += 1;
            continue;
        }
        if path[cursor..].starts_with("[id=") {
            let end = path[cursor + 4..]
                .find(']')
                .map(|offset| cursor + 4 + offset)
                .ok_or_else(|| format!("invalid conflict path '{path}'"))?;
            segments.push(ConflictPathSegment::Id(path[cursor + 4..end].to_owned()));
            cursor = end + 1;
            continue;
        }
        let end = path[cursor..]
            .find(['.', '['])
            .map(|offset| cursor + offset)
            .unwrap_or(path.len());
        let segment = &path[cursor..end];
        if segment.is_empty() {
            return Err(format!("invalid conflict path '{path}'"));
        }
        if let Ok(index) = segment.parse::<usize>() {
            segments.push(ConflictPathSegment::Index(index));
        } else {
            segments.push(ConflictPathSegment::Key(segment.to_owned()));
        }
        cursor = end;
    }
    Ok(segments)
}

fn set_conflicted_value(
    root: &mut Value,
    path: &str,
    replacement: Option<Value>,
) -> Result<(), String> {
    let segments = conflict_path_segments(path)?;
    let (last, parents) = segments
        .split_last()
        .ok_or_else(|| "conflict path cannot be empty".to_owned())?;
    let mut current = root;
    for segment in parents {
        current = match segment {
            ConflictPathSegment::Key(key) => current
                .as_object_mut()
                .and_then(|object| object.get_mut(key))
                .ok_or_else(|| format!("conflict path '{path}' is unavailable at '{key}'"))?,
            ConflictPathSegment::Index(index) => current
                .as_array_mut()
                .and_then(|array| array.get_mut(*index))
                .ok_or_else(|| format!("conflict path '{path}' is unavailable at index {index}"))?,
            ConflictPathSegment::Id(id) => current
                .as_array_mut()
                .and_then(|array| {
                    array
                        .iter_mut()
                        .find(|value| value.get("id").and_then(Value::as_str) == Some(id))
                })
                .ok_or_else(|| format!("conflict path '{path}' has no item with id '{id}'"))?,
        };
    }
    match (last, replacement) {
        (ConflictPathSegment::Key(key), Some(value)) => {
            current
                .as_object_mut()
                .ok_or_else(|| format!("conflict path '{path}' does not address an object"))?
                .insert(key.clone(), value);
        }
        (ConflictPathSegment::Key(key), None) => {
            current
                .as_object_mut()
                .ok_or_else(|| format!("conflict path '{path}' does not address an object"))?
                .remove(key);
        }
        (ConflictPathSegment::Index(index), Some(value)) => {
            let target = current
                .as_array_mut()
                .and_then(|array| array.get_mut(*index))
                .ok_or_else(|| format!("conflict path '{path}' has no index {index}"))?;
            *target = value;
        }
        (ConflictPathSegment::Index(index), None) => {
            let array = current
                .as_array_mut()
                .ok_or_else(|| format!("conflict path '{path}' does not address an array"))?;
            if *index >= array.len() {
                return Err(format!("conflict path '{path}' has no index {index}"));
            }
            array.remove(*index);
        }
        (ConflictPathSegment::Id(id), Some(value)) => {
            let target = current
                .as_array_mut()
                .and_then(|array| {
                    array
                        .iter_mut()
                        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
                })
                .ok_or_else(|| format!("conflict path '{path}' has no item with id '{id}'"))?;
            *target = value;
        }
        (ConflictPathSegment::Id(id), None) => {
            let array = current
                .as_array_mut()
                .ok_or_else(|| format!("conflict path '{path}' does not address an array"))?;
            let index = array
                .iter()
                .position(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
                .ok_or_else(|| format!("conflict path '{path}' has no item with id '{id}'"))?;
            array.remove(index);
        }
    }
    Ok(())
}

/// Merge `local` and `remote` given their common `ancestor`.
pub fn merge3(ancestor: &Value, local: &Value, remote: &Value) -> MergeOutcome {
    let mut conflicts = Vec::new();
    let merged = merge_values(ancestor, local, remote, "", &mut conflicts);
    if conflicts.is_empty() {
        MergeOutcome::Clean { merged }
    } else {
        MergeOutcome::Conflict {
            base: merged,
            conflicts,
        }
    }
}

/// Convenience wrapper that parses bytes before merging.
pub fn merge3_json_bytes(
    ancestor: &[u8],
    local: &[u8],
    remote: &[u8],
) -> Result<MergeOutcome, serde_json::Error> {
    let a: Value = serde_json::from_slice(ancestor)?;
    let l: Value = serde_json::from_slice(local)?;
    let r: Value = serde_json::from_slice(remote)?;
    Ok(merge3(&a, &l, &r))
}

// ---------------------------------------------------------------------------
// Internal recursive implementation
// ---------------------------------------------------------------------------

fn merge_values(
    ancestor: &Value,
    local: &Value,
    remote: &Value,
    path: &str,
    conflicts: &mut Vec<ConflictedField>,
) -> Value {
    match (ancestor, local, remote) {
        (Value::Object(a), Value::Object(l), Value::Object(r)) => {
            Value::Object(merge_objects(a, l, r, path, conflicts))
        }
        (Value::Array(a), Value::Array(l), Value::Array(r)) => {
            merge_arrays(a, l, r, path, conflicts)
        }
        _ => merge_scalar(ancestor, local, remote, path, conflicts),
    }
}

fn merge_objects(
    ancestor: &Map<String, Value>,
    local: &Map<String, Value>,
    remote: &Map<String, Value>,
    path: &str,
    conflicts: &mut Vec<ConflictedField>,
) -> Map<String, Value> {
    let mut all_keys: BTreeSet<&str> = BTreeSet::new();
    for k in ancestor.keys() {
        all_keys.insert(k.as_str());
    }
    for k in local.keys() {
        all_keys.insert(k.as_str());
    }
    for k in remote.keys() {
        all_keys.insert(k.as_str());
    }

    let mut out = Map::new();

    for key in all_keys {
        let child_path = child_path(path, key);
        let a_val = ancestor.get(key);
        let l_val = local.get(key);
        let r_val = remote.get(key);

        let l_changed = l_val != a_val;
        let r_changed = r_val != a_val;

        match (a_val, l_val, r_val) {
            // Both deleted.
            (_, None, None) => {}

            // Only local has it (new addition by local, or ancestor didn't have it).
            (None, Some(lv), None) => {
                out.insert(key.to_owned(), lv.clone());
            }
            // Only remote has it.
            (None, None, Some(rv)) => {
                out.insert(key.to_owned(), rv.clone());
            }

            // Local deleted, remote also deleted (covered above) or remote unchanged.
            (Some(_), None, Some(_)) if !r_changed => {
                // local deleted, remote unchanged → local wins deletion
            }
            // Local deleted, remote changed it → keep remote.
            (Some(_), None, Some(rv)) if r_changed => {
                out.insert(key.to_owned(), rv.clone());
            }
            // Remote deleted, local unchanged → remote wins deletion.
            (Some(_), Some(_), None) if !l_changed => {}
            // Remote deleted, local changed it → keep local.
            (Some(_), Some(lv), None) if l_changed => {
                out.insert(key.to_owned(), lv.clone());
            }

            // Both present, neither changed.
            (Some(av), Some(_), Some(_)) if !l_changed && !r_changed => {
                out.insert(key.to_owned(), av.clone());
            }
            // Only local changed.
            (Some(_), Some(lv), Some(_)) if l_changed && !r_changed => {
                out.insert(key.to_owned(), lv.clone());
            }
            // Only remote changed.
            (Some(_), Some(_), Some(rv)) if !l_changed && r_changed => {
                out.insert(key.to_owned(), rv.clone());
            }
            // Both changed — recurse to propagate any nested clean merges.
            (Some(av), Some(lv), Some(rv)) => {
                let merged_child = merge_values(av, lv, rv, &child_path, conflicts);
                out.insert(key.to_owned(), merged_child);
            }

            // Additions on both sides with the same value — idempotent.
            (None, Some(lv), Some(rv)) if lv == rv => {
                out.insert(key.to_owned(), lv.clone());
            }
            // Additions on both sides with different values — conflict.
            (None, Some(lv), Some(rv)) => {
                conflicts.push(ConflictedField {
                    path: child_path,
                    ancestor: None,
                    local: Some(lv.clone()),
                    remote: Some(rv.clone()),
                });
                out.insert(key.to_owned(), lv.clone());
            }

            // Exhaustive — all combinations covered above.
            _ => unreachable!(),
        }
    }

    out
}

fn merge_arrays(
    ancestor: &[Value],
    local: &[Value],
    remote: &[Value],
    path: &str,
    conflicts: &mut Vec<ConflictedField>,
) -> Value {
    // Attempt identity-based merge if ALL elements across all three arrays
    // carry a string "id" field.
    if all_have_id(ancestor) && all_have_id(local) && all_have_id(remote) {
        return merge_arrays_by_id(ancestor, local, remote, path, conflicts);
    }
    merge_arrays_by_index(ancestor, local, remote, path, conflicts)
}

fn all_have_id(arr: &[Value]) -> bool {
    arr.iter()
        .all(|v| matches!(v.get("id"), Some(Value::String(_))))
}

fn id_of(v: &Value) -> &str {
    v.get("id").and_then(Value::as_str).unwrap_or("")
}

fn merge_arrays_by_id(
    ancestor: &[Value],
    local: &[Value],
    remote: &[Value],
    path: &str,
    conflicts: &mut Vec<ConflictedField>,
) -> Value {
    // Build id-keyed maps for fast lookup.
    let ancestor_map: BTreeMap<&str, &Value> = ancestor.iter().map(|v| (id_of(v), v)).collect();
    let local_map: BTreeMap<&str, &Value> = local.iter().map(|v| (id_of(v), v)).collect();
    let remote_map: BTreeMap<&str, &Value> = remote.iter().map(|v| (id_of(v), v)).collect();

    let mut out: Vec<Value> = Vec::new();

    // First pass: ancestor-order for shared + modified elements.
    for av in ancestor {
        let id = id_of(av);
        let in_local = local_map.contains_key(id);
        let in_remote = remote_map.contains_key(id);

        match (in_local, in_remote) {
            (false, false) => {} // both deleted
            (true, false) => {
                // remote deleted; if local also kept it unchanged → delete
                // if local changed it → keep local
                let lv = local_map[id];
                if lv != av {
                    // local changed, remote deleted → keep local
                    out.push(lv.clone());
                }
                // else: local unchanged, remote deleted → exclude
            }
            (false, true) => {
                // local deleted; if remote kept unchanged → delete
                // if remote changed → keep remote
                let rv = remote_map[id];
                if rv != av {
                    out.push(rv.clone());
                }
            }
            (true, true) => {
                let lv = local_map[id];
                let rv = remote_map[id];
                let elem_path = format!("{path}[id={id}]");
                let merged = merge_values(av, lv, rv, &elem_path, conflicts);
                out.push(merged);
            }
        }
    }

    // Second pass: local-only additions (not in ancestor).
    for lv in local {
        let id = id_of(lv);
        if !ancestor_map.contains_key(id) {
            out.push(lv.clone());
        }
    }

    // Third pass: remote-only additions (not in ancestor, not in local).
    for rv in remote {
        let id = id_of(rv);
        if !ancestor_map.contains_key(id) && !local_map.contains_key(id) {
            out.push(rv.clone());
        }
    }

    Value::Array(out)
}

fn merge_arrays_by_index(
    ancestor: &[Value],
    local: &[Value],
    remote: &[Value],
    path: &str,
    conflicts: &mut Vec<ConflictedField>,
) -> Value {
    let len = ancestor.len().max(local.len()).max(remote.len());
    let null = Value::Null;
    let mut out = Vec::with_capacity(len);

    for i in 0..len {
        let av = ancestor.get(i).unwrap_or(&null);
        let lv = local.get(i).unwrap_or(&null);
        let rv = remote.get(i).unwrap_or(&null);
        let elem_path = format!("{path}.{i}");
        out.push(merge_values(av, lv, rv, &elem_path, conflicts));
    }

    Value::Array(out)
}

fn merge_scalar(
    ancestor: &Value,
    local: &Value,
    remote: &Value,
    path: &str,
    conflicts: &mut Vec<ConflictedField>,
) -> Value {
    let l_changed = local != ancestor;
    let r_changed = remote != ancestor;

    match (l_changed, r_changed) {
        (false, false) => ancestor.clone(),
        (true, false) => local.clone(),
        (false, true) => remote.clone(),
        (true, true) if local == remote => local.clone(), // both changed to same value
        (true, true) => {
            conflicts.push(ConflictedField {
                path: path.to_owned(),
                ancestor: Some(ancestor.clone()),
                local: Some(local.clone()),
                remote: Some(remote.clone()),
            });
            local.clone()
        }
    }
}

fn child_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_owned()
    } else {
        format!("{parent}.{key}")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clean_object_merge_disjoint_fields() {
        let ancestor = json!({"a": 1, "b": 2, "c": 3});
        let local = json!({"a": 99, "b": 2, "c": 3});
        let remote = json!({"a": 1, "b": 77, "c": 3});

        match merge3(&ancestor, &local, &remote) {
            MergeOutcome::Clean { merged } => {
                assert_eq!(merged["a"], 99);
                assert_eq!(merged["b"], 77);
                assert_eq!(merged["c"], 3);
            }
            MergeOutcome::Conflict { .. } => panic!("expected clean"),
        }
    }

    #[test]
    fn scalar_conflict() {
        let ancestor = json!({"x": 1});
        let local = json!({"x": 2});
        let remote = json!({"x": 3});

        match merge3(&ancestor, &local, &remote) {
            MergeOutcome::Conflict { conflicts, base } => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].path, "x");
                assert_eq!(base["x"], 2); // local kept in base
            }
            MergeOutcome::Clean { .. } => panic!("expected conflict"),
        }
    }

    #[test]
    fn conflict_resolution_applies_local_and_remote_choices_by_field() {
        let ancestor = serde_json::json!({"tempo": 120, "name": "Ancestor"});
        let local = serde_json::json!({"tempo": 128, "name": "Mine"});
        let remote = serde_json::json!({"tempo": 140, "name": "Theirs"});
        let MergeOutcome::Conflict { base, conflicts } = merge3(&ancestor, &local, &remote) else {
            panic!("fixture should conflict");
        };

        let resolved = resolve_conflicts(
            base,
            &conflicts,
            &[ConflictChoice::Local, ConflictChoice::Remote],
        )
        .expect("complete choices should resolve");

        assert_eq!(resolved["name"], "Mine");
        assert_eq!(resolved["tempo"], 140);
    }

    #[test]
    fn conflict_resolution_supports_identity_array_paths() {
        let ancestor = serde_json::json!({
            "channels": [{"id": "channel-1", "name": "Original", "volume": 1.0}]
        });
        let local = serde_json::json!({
            "channels": [{"id": "channel-1", "name": "Mine", "volume": 1.0}]
        });
        let remote = serde_json::json!({
            "channels": [{"id": "channel-1", "name": "Theirs", "volume": 1.0}]
        });
        let MergeOutcome::Conflict { base, conflicts } = merge3(&ancestor, &local, &remote) else {
            panic!("fixture should conflict");
        };
        assert_eq!(conflicts[0].path, "channels[id=channel-1].name");

        let resolved = resolve_conflicts(base, &conflicts, &[ConflictChoice::Remote])
            .expect("identity path should resolve");
        assert_eq!(resolved["channels"][0]["name"], "Theirs");
    }

    #[test]
    fn conflict_resolution_rejects_incomplete_choices() {
        let conflict = ConflictedField {
            path: "tempo".to_owned(),
            ancestor: Some(serde_json::json!(120)),
            local: Some(serde_json::json!(128)),
            remote: Some(serde_json::json!(140)),
        };

        let error = resolve_conflicts(serde_json::json!({"tempo": 128}), &[conflict], &[])
            .expect_err("missing choices must not silently prefer local");
        assert!(error.contains("expected 1 conflict choice"));
    }

    #[test]
    fn array_identity_merge() {
        // ancestor: channels [a, b]
        // local: channels [a(modified volume), b] + new channel c
        // remote: channels [a, b] + new channel d
        let ancestor = json!({
            "channels": [
                {"id": "a", "volume": 0.5},
                {"id": "b", "volume": 0.8}
            ]
        });
        let local = json!({
            "channels": [
                {"id": "a", "volume": 0.9},
                {"id": "b", "volume": 0.8},
                {"id": "c", "volume": 1.0}
            ]
        });
        let remote = json!({
            "channels": [
                {"id": "a", "volume": 0.5},
                {"id": "b", "volume": 0.8},
                {"id": "d", "volume": 0.3}
            ]
        });

        match merge3(&ancestor, &local, &remote) {
            MergeOutcome::Clean { merged } => {
                let channels = merged["channels"].as_array().unwrap();
                // a should have local's volume
                let a = channels.iter().find(|v| v["id"] == "a").unwrap();
                assert_eq!(a["volume"], 0.9);
                // b unchanged
                let b = channels.iter().find(|v| v["id"] == "b").unwrap();
                assert_eq!(b["volume"], 0.8);
                // both additions present
                assert!(channels.iter().any(|v| v["id"] == "c"));
                assert!(channels.iter().any(|v| v["id"] == "d"));
            }
            MergeOutcome::Conflict { .. } => panic!("expected clean"),
        }
    }

    #[test]
    fn array_index_fallback() {
        // Arrays without "id" fields fall back to index-based merge.
        let ancestor = json!({"tags": ["x", "y"]});
        let local = json!({"tags": ["z", "y"]});
        let remote = json!({"tags": ["x", "y"]});

        match merge3(&ancestor, &local, &remote) {
            MergeOutcome::Clean { merged } => {
                // local changed index 0, remote did not → local wins
                assert_eq!(merged["tags"][0], "z");
                assert_eq!(merged["tags"][1], "y");
            }
            MergeOutcome::Conflict { .. } => panic!("expected clean"),
        }
    }
}
