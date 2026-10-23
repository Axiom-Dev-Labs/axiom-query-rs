//! Parse frontend JSON domain expressions into [`QuerySpec`] AST nodes.

use serde_json::{Map, Value as JsonValue};

use crate::ast::{Condition, QuerySpec};
use crate::error::{ErrorCode, QueryError, Result};
use crate::op::Op;

/// Parse a domain expression into a [`QuerySpec`].
///
/// - `null` or `[]` matches everything.
/// - A list is an implicit AND of its items.
/// - An item is `[field_path, operator, value]` or a logical dict
///   `{"and": [...]}`, `{"or": [...]}`, `{"not": item}`.
pub fn parse_domain(raw: &JsonValue) -> Result<QuerySpec> {
    match raw {
        JsonValue::Null => Ok(QuerySpec::Bool(true)),
        JsonValue::Array(items) => parse_list(items),
        JsonValue::Object(map) => parse_dict(map),
        other => Err(invalid(format!(
            "Domain must be a list or dict, got {}",
            json_type_name(other)
        ))),
    }
}

fn parse_list(items: &[JsonValue]) -> Result<QuerySpec> {
    if items.is_empty() {
        return Ok(QuerySpec::Bool(true));
    }
    fold(items, QuerySpec::and)
}

fn fold(items: &[JsonValue], combine: fn(QuerySpec, QuerySpec) -> QuerySpec) -> Result<QuerySpec> {
    let mut specs = items.iter().map(parse_item);
    let first = specs.next().expect("caller checks for an empty list")?;
    specs.try_fold(first, |acc, spec| Ok(combine(acc, spec?)))
}

fn parse_item(item: &JsonValue) -> Result<QuerySpec> {
    match item {
        JsonValue::Array(triple) if triple.len() == 3 => {
            let field_path = triple[0].as_str().ok_or_else(|| {
                invalid(format!(
                    "Field path must be a string, got {}",
                    json_type_name(&triple[0])
                ))
            })?;
            let op = match &triple[1] {
                JsonValue::String(s) => s.parse::<Op>()?,
                other => return Err(invalid(format!("Unknown operator: {other}"))),
            };
            Ok(QuerySpec::Condition(Condition {
                field_path: field_path.to_owned(),
                op,
                value: triple[2].clone(),
            }))
        }
        JsonValue::Object(map) => parse_dict(map),
        other => Err(invalid(format!(
            "Each condition must be [field, op, value] or a logical dict, got {}",
            json_type_name(other)
        ))),
    }
}

fn parse_dict(map: &Map<String, JsonValue>) -> Result<QuerySpec> {
    let mut entries = map.iter();
    let (key, val) = match (entries.next(), entries.next()) {
        (Some(entry), None) => entry,
        _ => {
            return Err(invalid(
                "Logical dict must have exactly one key: 'and', 'or', or 'not'",
            ));
        }
    };

    match key.as_str() {
        "and" | "or" => {
            let items = match val {
                JsonValue::Array(items) if items.len() >= 2 => items,
                _ => {
                    return Err(invalid(format!(
                        "'{key}' requires a list of at least 2 items"
                    )));
                }
            };
            let combine = if key == "and" {
                QuerySpec::and
            } else {
                QuerySpec::or
            };
            fold(items, combine)
        }
        "not" => Ok(parse_item(val)?.negate()),
        _ => Err(invalid(format!(
            "Unknown logical key: {key:?}. Use 'and', 'or', or 'not'."
        ))),
    }
}

fn invalid(message: impl Into<String>) -> QueryError {
    QueryError::new(ErrorCode::InvalidDomain, message)
}

pub(crate) fn json_type_name(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "list",
        JsonValue::Object(_) => "dict",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn cond(field: &str, op: Op, value: JsonValue) -> QuerySpec {
        QuerySpec::condition(field, op, value)
    }

    #[test]
    fn null_and_empty_list_match_everything() {
        assert_eq!(
            parse_domain(&JsonValue::Null).unwrap(),
            QuerySpec::Bool(true)
        );
        assert_eq!(parse_domain(&json!([])).unwrap(), QuerySpec::Bool(true));
    }

    #[test]
    fn list_is_implicit_and() {
        let spec =
            parse_domain(&json!([["status", "=", "CONFIRMED"], ["total", ">", 100]])).unwrap();
        assert_eq!(
            spec,
            cond("status", Op::Eq, json!("CONFIRMED")) & cond("total", Op::Gt, json!(100))
        );
    }

    #[test]
    fn logical_dicts() {
        let spec =
            parse_domain(&json!({"or": [["a", "=", 1], ["b", "=", 2], ["c", "=", 3]]})).unwrap();
        assert_eq!(
            spec,
            (cond("a", Op::Eq, json!(1)) | cond("b", Op::Eq, json!(2)))
                | cond("c", Op::Eq, json!(3))
        );

        let spec = parse_domain(&json!({"not": ["status", "=", "CANCELLED"]})).unwrap();
        assert_eq!(spec, !cond("status", Op::Eq, json!("CANCELLED")));
    }

    #[test]
    fn list_mixes_conditions_and_dicts() {
        let spec = parse_domain(&json!([
            {"or": [["status", "=", "CONFIRMED"], ["status", "=", "DRAFT"]]},
            {"not": ["total", "=", 0]},
        ]))
        .unwrap();
        assert_eq!(
            spec,
            (cond("status", Op::Eq, json!("CONFIRMED")) | cond("status", Op::Eq, json!("DRAFT")))
                & !cond("total", Op::Eq, json!(0))
        );
    }

    #[test]
    fn rejects_malformed_domains() {
        for raw in [
            json!("status"),
            json!([["status", "="]]),
            json!([[1, "=", 2]]),
            json!([["status", "~", 2]]),
            json!([["status", 5, 2]]),
            json!({"and": [["a", "=", 1]]}),
            json!({"xor": [["a", "=", 1], ["b", "=", 2]]}),
            json!({"and": [], "or": []}),
            json!([42]),
        ] {
            let err = parse_domain(&raw).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidDomain, "{raw}");
        }
    }
}
