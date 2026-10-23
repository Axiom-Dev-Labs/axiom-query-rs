//! Compile a [`ReadGroupQuery`] into `SELECT ... GROUP BY ... HAVING`.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{Datelike, Duration, Months, NaiveDate};
use sea_orm::DbBackend;
use sea_orm::sea_query::{
    Asterisk, Condition, DynIden, Expr, ExprTrait, IntoIden, JoinType, Order, Query,
    SelectStatement,
};
use serde_json::{Map, Value as JsonValue, json};

use crate::aggregation::{
    AggFunc, AggregateSpec, DateGranularity, Direction, GroupBySpec, Pagination, ReadGroupQuery,
};
use crate::compiler::{Compiler, having_operator, join_condition, resolve_column, walk_ast};
use crate::error::{ErrorCode, QueryError, Result};
use crate::schema::{ColumnInfo, ModelSchema, RelationKind};
use crate::value::parse_group_date;

type JoinTree = BTreeMap<Vec<String>, (DynIden, Arc<ModelSchema>)>;

pub(crate) fn compile_read_group(
    compiler: &Compiler<'_>,
    query: &ReadGroupQuery,
    schema: &Arc<ModelSchema>,
    pagination: &Pagination,
) -> Result<SelectStatement> {
    let root = schema.table_iden();
    let mut stmt = Query::select();
    stmt.from(schema.table_ref.clone());

    let tree = build_join_tree(compiler, schema, query, &mut stmt)?;

    let mut aliases: Vec<(String, Expr)> = Vec::new();
    for g in &query.groupby {
        let expr = groupby_column(compiler.backend(), g, &tree)?;
        aliases.push((g.alias(), expr));
    }
    let groupby_count = aliases.len();
    for a in &query.aggregates {
        aliases.push((a.alias.clone(), aggregate_column(a, &tree)?));
    }
    for (alias, expr) in &aliases {
        stmt.expr_as(expr.clone(), alias.clone().into_iden());
    }

    if let Some(domain) = &query.domain {
        stmt.cond_where(compiler.domain(domain, schema, &root)?);
    }

    for (_, expr) in &aliases[..groupby_count] {
        stmt.add_group_by([expr.clone()]);
    }

    let lookup = |alias: &str, code: ErrorCode, what: &str| -> Result<Expr> {
        aliases
            .iter()
            .find(|(a, _)| a == alias)
            .map(|(_, e)| e.clone())
            .ok_or_else(|| {
                let mut available: Vec<_> = aliases.iter().map(|(a, _)| a.as_str()).collect();
                available.sort_unstable();
                QueryError::new(
                    code,
                    format!(
                        "{what} '{alias}' is not a groupby or aggregate alias. Available: {}",
                        available.join(", ")
                    ),
                )
            })
    };

    if let Some(having) = &query.having {
        let condition = walk_ast(having, &mut |alias, op, value| {
            let expr = lookup(alias, ErrorCode::InvalidHavingField, "HAVING field")?;
            Ok(Condition::all().add(having_operator(expr, op, value)?))
        })?;
        stmt.cond_having(condition);
    }

    for order in &pagination.order_by {
        let expr = lookup(&order.field, ErrorCode::InvalidOrderBy, "ORDER BY alias")?;
        stmt.order_by_expr(expr, order_direction(order.direction));
    }
    if let Some(limit) = pagination.limit {
        stmt.limit(limit);
    }
    if let Some(offset) = pagination.offset {
        stmt.offset(offset);
    }
    Ok(stmt)
}

pub(crate) fn order_direction(direction: Direction) -> Order {
    match direction {
        Direction::Asc => Order::Asc,
        Direction::Desc => Order::Desc,
    }
}

/// LEFT JOIN every relation-path prefix once; divergent paths get distinct aliases.
///
/// More than one one-to-many edge would fan out and double-count aggregates,
/// so it is rejected.
fn build_join_tree(
    compiler: &Compiler<'_>,
    schema: &Arc<ModelSchema>,
    query: &ReadGroupQuery,
    stmt: &mut SelectStatement,
) -> Result<JoinTree> {
    let mut tree = JoinTree::new();
    tree.insert(Vec::new(), (schema.table_iden(), Arc::clone(schema)));
    let mut one_to_many = 0;

    let mut paths: Vec<_> = query.referenced_relation_paths().into_iter().collect();
    paths.sort_by_key(Vec::len);

    for path in paths {
        for depth in 1..=path.len() {
            let prefix = &path[..depth];
            if tree.contains_key(prefix) {
                continue;
            }
            let (parent_iden, parent) = tree[&prefix[..depth - 1]].clone();
            let segment = &prefix[depth - 1];
            let relation = parent.relation(segment).ok_or_else(|| {
                QueryError::new(
                    ErrorCode::InvalidFilterField,
                    format!("No relation '{segment}' on {}", parent.table),
                )
            })?;
            if relation.kind == RelationKind::OneToMany {
                one_to_many += 1;
            }
            let related = compiler.schema(&relation.target)?;
            let alias = compiler.next_alias(related.table);
            stmt.join_as(
                JoinType::LeftJoin,
                relation.def.to_tbl.clone(),
                alias.clone(),
                join_condition(relation, parent_iden, alias.clone()),
            );
            tree.insert(prefix.to_vec(), (alias, related));
        }
    }

    if one_to_many > 1 {
        return Err(QueryError::new(
            ErrorCode::MultiChildAggregation,
            "Aggregating across multiple one-to-many relations simultaneously is not supported \
             (the join would fan out and double-count). Aggregate one one-to-many branch at a time.",
        ));
    }
    Ok(tree)
}

fn resolve_agg_column<'t>(field_path: &str, tree: &'t JoinTree) -> Result<(Expr, &'t ColumnInfo)> {
    let segments: Vec<String> = field_path.split('.').map(str::to_owned).collect();
    let (leaf, prefix) = segments
        .split_last()
        .expect("split always yields a segment");
    let (iden, schema) = tree.get(prefix).ok_or_else(|| {
        QueryError::new(
            ErrorCode::InvalidFilterField,
            format!("No relation path '{}'", prefix.join(".")),
        )
    })?;
    let column = resolve_column(schema, leaf)?;
    Ok((Expr::col((iden.clone(), column.name.into_iden())), column))
}

fn groupby_column(backend: DbBackend, spec: &GroupBySpec, tree: &JoinTree) -> Result<Expr> {
    let (col, column) = resolve_agg_column(&spec.field_path, tree)?;
    let Some(granularity) = spec.granularity else {
        return Ok(col);
    };
    if !column.is_temporal() {
        return Err(QueryError::new(
            ErrorCode::InvalidAggregation,
            format!(
                "Date granularity requires a date/datetime field. '{}' is not a date type.",
                spec.field_path
            ),
        ));
    }
    Ok(date_trunc(backend, granularity, col))
}

/// Truncate to the first day of the period, rendered as a `YYYY-MM-DD` date on every backend.
///
/// The granularity is spliced in as a literal (it comes from a closed enum), so
/// the SELECT and GROUP BY expressions are textually identical — Postgres rejects
/// them as different when each carries its own bind parameter. Postgres numbers
/// placeholders (`$1`); SQLite and MySQL fill `?` in order.
fn date_trunc(backend: DbBackend, granularity: DateGranularity, col: Expr) -> Expr {
    use DateGranularity::*;
    let sql: &str = match backend {
        DbBackend::Postgres => {
            return Expr::cust_with_expr(
                format!("CAST(DATE_TRUNC('{granularity}', $1) AS DATE)"),
                col,
            );
        }
        DbBackend::Sqlite => match granularity {
            Day => "date(?)",
            Week => "date(?, '-6 days', 'weekday 1')",
            Month => "strftime('%Y-%m-01', ?)",
            Quarter => {
                "printf('%s-%02d-01', strftime('%Y', ?), ((CAST(strftime('%m', ?) AS INTEGER) - 1) / 3) * 3 + 1)"
            }
            Year => "strftime('%Y-01-01', ?)",
        },
        _ => match granularity {
            Day => "DATE(?)",
            Week => "DATE_SUB(DATE(?), INTERVAL WEEKDAY(?) DAY)",
            Month => "CAST(DATE_FORMAT(?, '%Y-%m-01') AS DATE)",
            Quarter => "MAKEDATE(YEAR(?), 1) + INTERVAL (QUARTER(?) - 1) QUARTER",
            Year => "MAKEDATE(YEAR(?), 1)",
        },
    };
    Expr::cust_with_exprs(sql, [col.clone(), col])
}

fn aggregate_column(spec: &AggregateSpec, tree: &JoinTree) -> Result<Expr> {
    let Some(field_path) = &spec.field_path else {
        return Ok(Expr::col(Asterisk).count());
    };
    let (col, column) = resolve_agg_column(field_path, tree)?;
    if matches!(spec.function, AggFunc::Sum | AggFunc::Avg) && !column.is_numeric() {
        return Err(QueryError::new(
            ErrorCode::InvalidAggregation,
            format!("SUM/AVG requires a numeric field. '{field_path}' is not numeric."),
        ));
    }
    Ok(match spec.function {
        AggFunc::Count => col.count(),
        AggFunc::Sum => col.sum(),
        AggFunc::Avg => col.avg(),
        AggFunc::Min => col.min(),
        AggFunc::Max => col.max(),
    })
}

/// Build a domain selecting exactly the records in one group (`__domain`).
///
/// Pass it back to `list()` / `count()` to drill down into the group.
pub fn build_group_domain(
    group: &Map<String, JsonValue>,
    groupby: &[GroupBySpec],
    original_domain: Option<&JsonValue>,
) -> JsonValue {
    let mut conditions = Vec::new();
    for spec in groupby {
        let value = group.get(&spec.alias()).unwrap_or(&JsonValue::Null);
        match (value, spec.granularity) {
            (JsonValue::Null, _) => conditions.push(json!([spec.field_path, "is_null", true])),
            (value, Some(granularity)) => {
                let (start, end) = date_range_bounds(value, granularity);
                conditions.push(json!([spec.field_path, ">=", start]));
                conditions.push(json!([spec.field_path, "<", end]));
            }
            (value, None) => conditions.push(json!([spec.field_path, "=", value])),
        }
    }
    match original_domain {
        None | Some(JsonValue::Null) => {}
        Some(JsonValue::Array(items)) => conditions.extend(items.iter().cloned()),
        Some(other) => conditions.push(other.clone()),
    }
    JsonValue::Array(conditions)
}

/// `[start, end)` ISO date bounds for a date-truncated group value.
fn date_range_bounds(value: &JsonValue, granularity: DateGranularity) -> (String, String) {
    let parsed = value.as_str().and_then(parse_group_date);
    let Some(date) = parsed else {
        let raw = value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned);
        return (raw.clone(), raw);
    };
    let first_of_month = |d: NaiveDate| d.with_day(1).unwrap_or(d);
    let (start, end) = match granularity {
        DateGranularity::Day => (date, date + Duration::days(1)),
        DateGranularity::Week => (date, date + Duration::weeks(1)),
        DateGranularity::Month => {
            let start = first_of_month(date);
            (start, start + Months::new(1))
        }
        DateGranularity::Quarter => {
            let start = first_of_month(date);
            (start, start + Months::new(3))
        }
        DateGranularity::Year => {
            let start = NaiveDate::from_ymd_opt(date.year(), 1, 1).unwrap_or(date);
            (start, start + Months::new(12))
        }
    };
    (start.to_string(), end.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_bounds_roll_over_year_end() {
        assert_eq!(
            date_range_bounds(&json!("2026-12-01"), DateGranularity::Month),
            ("2026-12-01".to_owned(), "2027-01-01".to_owned())
        );
        assert_eq!(
            date_range_bounds(&json!("2026-10-01"), DateGranularity::Quarter),
            ("2026-10-01".to_owned(), "2027-01-01".to_owned())
        );
        assert_eq!(
            date_range_bounds(&json!("2026-03-15 00:00:00"), DateGranularity::Year),
            ("2026-01-01".to_owned(), "2027-01-01".to_owned())
        );
    }

    #[test]
    fn group_domain_combines_groups_and_original() {
        let group = json!({"status": "CONFIRMED", "created_at__month": "2026-01-01", "customer__name": null});
        let groupby = [
            GroupBySpec {
                field_path: "status".into(),
                granularity: None,
            },
            GroupBySpec {
                field_path: "created_at".into(),
                granularity: Some(DateGranularity::Month),
            },
            GroupBySpec {
                field_path: "customer.name".into(),
                granularity: None,
            },
        ];
        let domain = build_group_domain(
            group.as_object().unwrap(),
            &groupby,
            Some(&json!({"not": ["total", "=", 0]})),
        );
        assert_eq!(
            domain,
            json!([
                ["status", "=", "CONFIRMED"],
                ["created_at", ">=", "2026-01-01"],
                ["created_at", "<", "2026-02-01"],
                ["customer.name", "is_null", true],
                {"not": ["total", "=", 0]},
            ])
        );
    }
}
