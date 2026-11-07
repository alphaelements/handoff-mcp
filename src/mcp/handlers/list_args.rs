//! Shared parsing for the narrowing arguments of the list tools
//! (`handoff_doc_list`, `handoff_list_tasks`): `limit` and `fields`.
//!
//! Both are optional; a missing or JSON `null` value means "do not narrow",
//! so a caller that omits them gets exactly the pre-existing output.

use anyhow::{bail, Result};
use serde_json::Value;

/// Reads `limit`: absent or `null` -> `None` (no cap); otherwise it must be a
/// JSON integer >= 1.
pub(crate) fn parse_limit(arguments: &Value) -> Result<Option<usize>> {
    match arguments.get("limit") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if n >= 1 => Ok(Some(usize::try_from(n).unwrap_or(usize::MAX))),
            _ => bail!("'limit' must be an integer >= 1 (got {v})"),
        },
    }
}

/// Reads `fields` against the `allowed` list.
///
/// Absent, `null` or an empty array -> `None` (no projection). Otherwise the
/// names are de-duplicated (first occurrence wins) and every name must be in
/// `allowed`; an unknown name errors with the full allowed list so the caller
/// can correct the call without a second lookup.
pub(crate) fn parse_fields(arguments: &Value, allowed: &[&str]) -> Result<Option<Vec<String>>> {
    let items = match arguments.get("fields") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(items)) => items,
        Some(other) => bail!("'fields' must be an array of strings (got {other})"),
    };
    let mut names: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.as_str() else {
            bail!("'fields' must be an array of strings (got element {item})");
        };
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    if names.is_empty() {
        return Ok(None);
    }
    let unknown: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| !allowed.contains(n))
        .collect();
    if !unknown.is_empty() {
        bail!(
            "Unknown field(s) in 'fields': {}. Allowed fields: {}",
            unknown.join(", "),
            allowed.join(", ")
        );
    }
    Ok(Some(names))
}

/// Keeps only `keep` keys of a JSON object (a non-object is returned as is),
/// preserving the object's own key order.
pub(crate) fn project_object(value: Value, keep: impl Fn(&str) -> bool) -> Value {
    match value {
        Value::Object(map) => Value::Object(map.into_iter().filter(|(k, _)| keep(k)).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALLOWED: &[&str] = &["id", "title", "status"];

    #[test]
    fn limit_absent_or_null_is_unbounded() {
        assert_eq!(parse_limit(&json!({})).unwrap(), None);
        assert_eq!(parse_limit(&json!({ "limit": null })).unwrap(), None);
    }

    #[test]
    fn limit_accepts_positive_integers_only() {
        assert_eq!(parse_limit(&json!({ "limit": 3 })).unwrap(), Some(3));
        for bad in [json!(0), json!(-2), json!(1.5), json!("3"), json!(true)] {
            let err = parse_limit(&json!({ "limit": bad }))
                .unwrap_err()
                .to_string();
            assert!(err.contains("'limit' must be an integer >= 1"), "{err}");
        }
    }

    #[test]
    fn fields_absent_null_or_empty_means_no_projection() {
        assert_eq!(parse_fields(&json!({}), ALLOWED).unwrap(), None);
        assert_eq!(
            parse_fields(&json!({ "fields": null }), ALLOWED).unwrap(),
            None
        );
        assert_eq!(
            parse_fields(&json!({ "fields": [] }), ALLOWED).unwrap(),
            None
        );
    }

    #[test]
    fn fields_are_deduplicated_in_first_seen_order() {
        let got = parse_fields(&json!({ "fields": ["status", "id", "status"] }), ALLOWED)
            .unwrap()
            .unwrap();
        assert_eq!(got, vec!["status", "id"]);
    }

    #[test]
    fn fields_unknown_names_are_all_listed_with_the_allowed_set() {
        let err = parse_fields(&json!({ "fields": ["foo", "id", "bar"] }), ALLOWED)
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "Unknown field(s) in 'fields': foo, bar. Allowed fields: id, title, status"
        );
    }

    #[test]
    fn fields_reject_non_array_and_non_string_elements() {
        assert!(parse_fields(&json!({ "fields": "id" }), ALLOWED).is_err());
        assert!(parse_fields(&json!({ "fields": ["id", 3] }), ALLOWED).is_err());
    }

    #[test]
    fn project_object_keeps_selected_keys_in_original_order() {
        let v = json!({ "a": 1, "b": 2, "c": 3 });
        let out = project_object(v, |k| k != "b");
        assert_eq!(serde_json::to_string(&out).unwrap(), r#"{"a":1,"c":3}"#);
    }
}
