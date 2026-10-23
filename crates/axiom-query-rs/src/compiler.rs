//! Compile a [`QuerySpec`] AST into a sea-query WHERE [`Condition`].

use std::cell::Cell;
use std::sync::Arc;

use sea_orm::DbBackend;
use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{
    Alias, Condition, ConditionType, DynIden, Expr, ExprTrait, Func, IntoIden, LikeExpr, Query,
    Value,
};
use serde_json::Value as JsonValue;

use crate::ast::QuerySpec;
use crate::error::{ErrorCode, QueryError, Result};
use crate::op::Op;
use crate::parser::json_type_name;
use crate::schema::{ColumnInfo, ModelSchema, Registry, RelationInfo};
use crate::value::{column_value, plain_value};

/// Walk a [`QuerySpec`], resolving each leaf condition with `resolve`.
pub(crate) fn walk_ast(
    spec: &QuerySpec,
    resolve: &mut dyn FnMut(&str, Op, &JsonValue) -> Result<Condition>,
) -> Result<Condition> {
    Ok(match spec {
        QuerySpec::Bool(true) => Condition::all().add(Expr::cust("1 = 1")),
        QuerySpec::Bool(false) => Condition::all().add(Expr::cust("1 = 0")),
        QuerySpec::And(left, right) => Condition::all()
            .add(walk_ast(left, resolve)?)
            .add(walk_ast(right, resolve)?),
        QuerySpec::Or(left, right) => Condition::any()
            .add(walk_ast(left, resolve)?)
            .add(walk_ast(right, resolve)?),
        QuerySpec::Not(operand) => walk_ast(operand, resolve)?.not(),
        QuerySpec::Condition(c) => resolve(&c.field_path, c.op, &c.value)?,
    })
}

/// Compiles domains against a root entity, traversing relations through the registry.
pub(crate) struct Compiler<'a> {
    registry: &'a Registry,
    root: &'a Arc<ModelSchema>,
    backend: DbBackend,
    alias_seq: Cell<usize>,
}

impl<'a> Compiler<'a> {
    pub(crate) fn new(
        registry: &'a Registry,
        root: &'a Arc<ModelSchema>,
        backend: DbBackend,
    ) -> Self {
        Self {
            registry,
            root,
            backend,
            alias_seq: Cell::new(0),
        }
    }

    pub(crate) fn backend(&self) -> DbBackend {
        self.backend
    }

    /// Compile `spec` against `schema`, whose rows are referenced as `target`.
    pub(crate) fn domain(
        &self,
        spec: &QuerySpec,
        schema: &ModelSchema,
        target: &DynIden,
    ) -> Result<Condition> {
        walk_ast(spec, &mut |field_path, op, value| {
            let segments: Vec<&str> = field_path.split('.').collect();
            self.path_condition(schema, target, &segments, op, value)
                .map(|expr| Condition::all().add(expr))
        })
    }

    /// Resolve a (possibly multi-hop) relational path into a condition.
    ///
    /// The leaf segment is a scalar column; each preceding segment is a relation,
    /// compiled as a correlated `EXISTS` over a fresh alias using the relation's
    /// full join condition (including any `on_condition`).
    fn path_condition(
        &self,
        schema: &ModelSchema,
        target: &DynIden,
        segments: &[&str],
        op: Op,
        value: &JsonValue,
    ) -> Result<Expr> {
        let (head, rest) = segments
            .split_first()
            .expect("split always yields a segment");

        if rest.is_empty() {
            let column = resolve_column(schema, head)?;
            let expr = Expr::col((target.clone(), column.name.into_iden()));
            return self.apply_operator(expr, column, op, value, segments.join(".").as_str());
        }

        let relation = schema.relation(head).ok_or_else(|| {
            QueryError::new(
                ErrorCode::InvalidFilterField,
                format!(
                    "No relation '{head}' on {}. Available: {}",
                    schema.table,
                    schema.relation_names()
                ),
            )
        })?;
        let related = self.schema(&relation.target)?;
        let alias = self.next_alias(related.table);
        let inner = self.path_condition(&related, &alias, rest, op, value)?;

        let mut subquery = Query::select();
        subquery
            .expr(Expr::cust("1"))
            .from_as(relation.def.to_tbl.clone(), alias.clone())
            .cond_where(join_condition(relation, target.clone(), alias).add(inner));
        Ok(Expr::exists(subquery))
    }

    pub(crate) fn schema(&self, key: &str) -> Result<Arc<ModelSchema>> {
        if key == self.root.key {
            return Ok(Arc::clone(self.root));
        }
        self.registry.get(key).cloned().ok_or_else(|| {
            QueryError::new(
                ErrorCode::UnregisteredEntity,
                format!("Entity for table '{key}' is not registered"),
            )
        })
    }

    pub(crate) fn next_alias(&self, table: &str) -> DynIden {
        let n = self.alias_seq.get() + 1;
        self.alias_seq.set(n);
        Alias::new(format!("{table}_{n}")).into_iden()
    }

    fn apply_operator(
        &self,
        col: Expr,
        column: &ColumnInfo,
        op: Op,
        value: &JsonValue,
        field_path: &str,
    ) -> Result<Expr> {
        let typed = |v: &JsonValue| -> Result<Expr> {
            let value = column_value(column, field_path, v)?;
            Ok(self.bind(column, value))
        };
        let ordered = matches!(op, Op::Gt | Op::Lt | Op::Gte | Op::Lte);
        if ordered && value.is_null() {
            return Err(QueryError::new(
                ErrorCode::InvalidFilterValue,
                format!("Operator '{op}' on '{field_path}' does not accept null; use 'is_null'"),
            ));
        }

        Ok(match op {
            Op::Eq if value.is_null() => col.is_null(),
            Op::Ne if value.is_null() => col.is_not_null(),
            Op::Eq => col.eq(typed(value)?),
            Op::Ne => col.ne(typed(value)?),
            Op::Gt => col.gt(typed(value)?),
            Op::Lt => col.lt(typed(value)?),
            Op::Gte => col.gte(typed(value)?),
            Op::Lte => col.lte(typed(value)?),
            Op::In | Op::NotIn => {
                let items = list_value(value, op, field_path)?
                    .iter()
                    .map(typed)
                    .collect::<Result<Vec<_>>>()?;
                if op == Op::In {
                    col.is_in(items)
                } else {
                    col.is_not_in(items)
                }
            }
            Op::Like
            | Op::Ilike
            | Op::Contains
            | Op::IContains
            | Op::StartsWith
            | Op::IStartsWith
            | Op::EndsWith
            | Op::IEndsWith => {
                let raw = value.as_str().ok_or_else(|| {
                    QueryError::new(
                        ErrorCode::InvalidFilterValue,
                        format!("Operator '{op}' on '{field_path}' requires a string value"),
                    )
                })?;
                self.text_match(col, op, raw)
            }
            Op::IsNull => match value {
                JsonValue::Bool(true) => col.is_null(),
                JsonValue::Bool(false) => col.is_not_null(),
                other => {
                    return Err(QueryError::new(
                        ErrorCode::InvalidFilterValue,
                        format!("'is_null' on '{field_path}' requires true or false, got {other}"),
                    ));
                }
            },
        })
    }

    /// `LIKE` / `ILIKE` for a raw pattern (`like`, `ilike`) or an escaped literal
    /// (`contains`, `starts_with`, `ends_with` and their `i` variants).
    fn text_match(&self, col: Expr, op: Op, raw: &str) -> Expr {
        let pattern = match op {
            Op::Contains | Op::IContains => format!("%{}%", escape_like(raw)),
            Op::StartsWith | Op::IStartsWith => format!("{}%", escape_like(raw)),
            Op::EndsWith | Op::IEndsWith => format!("%{}", escape_like(raw)),
            _ => raw.to_owned(),
        };
        // Postgres already escapes with `\\` by default, and sea-query renders an
        // explicit ESCAPE after ILIKE as `ILIKE (pattern ESCAPE ...)`, which Postgres rejects.
        let escaped = !matches!(op, Op::Like | Op::Ilike) && self.backend != DbBackend::Postgres;
        let like = |pattern: String| {
            let expr = LikeExpr::new(pattern);
            if escaped { expr.escape('\\') } else { expr }
        };
        match (op.is_case_insensitive(), self.backend) {
            (false, _) => col.like(like(pattern)),
            (true, DbBackend::Postgres) => col.ilike(like(pattern)),
            (true, _) => Expr::from(Func::lower(col)).like(like(pattern.to_lowercase())),
        }
    }

    /// Bind a value, casting to the column's database enum type on Postgres.
    fn bind(&self, column: &ColumnInfo, value: Value) -> Expr {
        match (column.enum_type_name, self.backend) {
            (Some(name), DbBackend::Postgres) => Expr::val(value).as_enum(Alias::new(name)),
            _ => Expr::val(value),
        }
    }
}

/// The relation's join condition between `from` (declaring side) and `to`.
pub(crate) fn join_condition(relation: &RelationInfo, from: DynIden, to: DynIden) -> Condition {
    let def = &relation.def;
    let mut condition = match def.condition_type {
        ConditionType::All => Condition::all(),
        ConditionType::Any => Condition::any(),
    };
    for (from_col, to_col) in def.from_col.clone().into_iter().zip(def.to_col.clone()) {
        condition = condition.add(Expr::col((from.clone(), from_col)).equals((to.clone(), to_col)));
    }
    if let Some(extra) = &def.on_condition {
        condition = condition.add(extra(from, to));
    }
    Condition::all().add(condition)
}

pub(crate) fn resolve_column<'s>(schema: &'s ModelSchema, name: &str) -> Result<&'s ColumnInfo> {
    schema.column(name).ok_or_else(|| {
        QueryError::new(
            ErrorCode::InvalidFilterField,
            format!(
                "No field '{name}' on {}. Available: {}",
                schema.table,
                schema.column_names()
            ),
        )
    })
}

/// Escape `LIKE` wildcards so the value matches literally (escape character `\\`).
fn escape_like(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn list_value<'v>(value: &'v JsonValue, op: Op, field_path: &str) -> Result<&'v [JsonValue]> {
    value.as_array().map(Vec::as_slice).ok_or_else(|| {
        QueryError::new(
            ErrorCode::InvalidFilterValue,
            format!(
                "Operator '{op}' on '{field_path}' requires a list, got {}",
                json_type_name(value)
            ),
        )
    })
}

/// Apply a comparison operator in a HAVING clause (no column type to coerce to).
pub(crate) fn having_operator(expr: Expr, op: Op, value: &JsonValue) -> Result<Expr> {
    let scalar = |v: &JsonValue| plain_value(v).map(Expr::val);
    Ok(match op {
        Op::Eq if value.is_null() => expr.is_null(),
        Op::Ne if value.is_null() => expr.is_not_null(),
        Op::Eq => expr.eq(scalar(value)?),
        Op::Ne => expr.ne(scalar(value)?),
        Op::Gt => expr.gt(scalar(value)?),
        Op::Lt => expr.lt(scalar(value)?),
        Op::Gte => expr.gte(scalar(value)?),
        Op::Lte => expr.lte(scalar(value)?),
        Op::In | Op::NotIn => {
            let items = match value {
                JsonValue::Array(items) => items.iter().map(scalar).collect::<Result<Vec<_>>>()?,
                other => vec![scalar(other)?],
            };
            if op == Op::In {
                expr.is_in(items)
            } else {
                expr.is_not_in(items)
            }
        }
        _ => {
            return Err(QueryError::new(
                ErrorCode::InvalidHavingOperator,
                format!("Operator '{op}' is not supported in HAVING clauses"),
            ));
        }
    })
}
