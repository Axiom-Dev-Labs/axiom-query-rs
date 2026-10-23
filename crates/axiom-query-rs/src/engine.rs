//! [`QueryEngine`] — public facade.

use std::marker::PhantomData;
use std::sync::Arc;

use futures_util::{Stream, TryStreamExt};
use sea_orm::prelude::Decimal;
use sea_orm::sea_query::{Asterisk, Condition, Expr, ExprTrait, IntoIden, Query, SelectStatement};
use sea_orm::{
    ConnectionTrait, DbBackend, DbErr, EntityTrait, FromQueryResult, QueryFilter, QueryResult,
    QueryTrait, Select, StreamTrait, TryGetable,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};

use crate::aggregation::{OrderBy, ReadGroupParams};
use crate::compiler::{Compiler, resolve_column};
use crate::compiler_aggregate::{build_group_domain, compile_read_group, order_direction};
use crate::error::{ErrorCode, QueryError, Result};
use crate::parser::parse_domain;
use crate::schema::{ModelSchema, Registry};

/// One `read_group` result row, keyed by groupby / aggregate alias, plus `__domain`.
pub type Group = Map<String, JsonValue>;

/// Parameters for [`QueryEngine::list`], deserializable from a JSON body.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListParams {
    pub domain: Option<JsonValue>,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

impl ListParams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn domain(mut self, domain: JsonValue) -> Self {
        self.domain = Some(domain);
        self
    }

    pub fn order_by(mut self, order: OrderBy) -> Self {
        self.order_by.push(order);
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }
}

/// Specification-based query engine for a SeaORM entity.
///
/// ```ignore
/// let registry = Arc::new(Registry::new().register::<customer::Entity>().register::<order_line::Entity>());
/// let engine = QueryEngine::<order::Entity>::new(registry);
///
/// let page = engine.list(&db, &ListParams::new().domain(json!([["status", "=", "CONFIRMED"]])).limit(20)).await?;
/// let n = engine.count(&db, Some(&json!([["lines.quantity", ">", 2]]))).await?;
/// let groups = engine.read_group(&db, &ReadGroupParams::new(["status"], ["__count"])).await?;
/// ```
pub struct QueryEngine<E: EntityTrait> {
    schema: Arc<ModelSchema>,
    registry: Arc<Registry>,
    _entity: PhantomData<fn() -> E>,
}

impl<E: EntityTrait> Clone for QueryEngine<E> {
    fn clone(&self) -> Self {
        Self {
            schema: Arc::clone(&self.schema),
            registry: Arc::clone(&self.registry),
            _entity: PhantomData,
        }
    }
}

impl<E: EntityTrait> std::fmt::Debug for QueryEngine<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryEngine")
            .field("table", &self.schema.key)
            .finish_non_exhaustive()
    }
}

impl<E: EntityTrait> QueryEngine<E> {
    /// Create an engine for `E`. `registry` holds every entity that relation
    /// paths may traverse into; `E` itself need not be registered.
    pub fn new(registry: Arc<Registry>) -> Self {
        let schema = ModelSchema::of::<E>();
        let schema = registry
            .get(&schema.key)
            .cloned()
            .unwrap_or_else(|| Arc::new(schema));
        Self {
            schema,
            registry,
            _entity: PhantomData,
        }
    }

    pub fn schema(&self) -> &ModelSchema {
        &self.schema
    }

    fn compiler(&self, backend: DbBackend) -> Compiler<'_> {
        Compiler::new(&self.registry, &self.schema, backend)
    }

    /// Compile a domain into a WHERE condition on `E`'s table, for use in your own queries.
    pub fn condition(&self, domain: &JsonValue, backend: DbBackend) -> Result<Condition> {
        let spec = parse_domain(domain)?;
        self.compiler(backend)
            .domain(&spec, &self.schema, &self.schema.table_iden())
    }

    /// Build the `SELECT` that [`list`](Self::list) runs.
    pub fn select(&self, params: &ListParams, backend: DbBackend) -> Result<Select<E>> {
        let mut select = E::find();
        if let Some(domain) = &params.domain {
            select = select.filter(self.condition(domain, backend)?);
        }
        let table = self.schema.table_iden();
        let query = QueryTrait::query(&mut select);
        for order in &params.order_by {
            let column = resolve_column(&self.schema, &order.field)
                .map_err(|err| QueryError::new(ErrorCode::InvalidOrderBy, err.message()))?;
            query.order_by_expr(
                Expr::col((table.clone(), column.name.into_iden())),
                order_direction(order.direction),
            );
        }
        if let Some(limit) = params.limit {
            query.limit(limit);
        }
        if let Some(offset) = params.offset {
            query.offset(offset);
        }
        Ok(select)
    }

    /// Return all records matching `params` (materialised; see [`search`](Self::search) to stream).
    pub async fn list<C: ConnectionTrait>(
        &self,
        db: &C,
        params: &ListParams,
    ) -> Result<Vec<E::Model>> {
        let select = self.select(params, db.get_database_backend())?;
        Ok(select.all(db).await?)
    }

    /// Stream every matching record. No limit/offset: this is for processing all matches.
    ///
    /// The stream holds a pooled connection until it is dropped; drop it before
    /// issuing further queries on a single-connection pool (e.g. in-memory SQLite).
    pub async fn search<'a, C>(
        &self,
        db: &'a C,
        domain: Option<&JsonValue>,
        order_by: &[OrderBy],
    ) -> Result<impl Stream<Item = Result<E::Model>> + Send + 'a>
    where
        C: ConnectionTrait + StreamTrait + Send,
        E::Model: Send + 'a,
    {
        let params = ListParams {
            domain: domain.cloned(),
            order_by: order_by.to_vec(),
            ..ListParams::default()
        };
        let select = self.select(&params, ConnectionTrait::get_database_backend(db))?;
        Ok(select.stream(db).await?.map_err(QueryError::from))
    }

    /// Build the `SELECT COUNT(*)` that [`count`](Self::count) runs.
    pub fn count_statement(
        &self,
        domain: Option<&JsonValue>,
        backend: DbBackend,
    ) -> Result<SelectStatement> {
        let mut stmt = Query::select();
        stmt.expr_as(Expr::col(Asterisk).count(), "count".into_iden())
            .from(self.schema.table_ref.clone());
        if let Some(domain) = domain {
            stmt.cond_where(self.condition(domain, backend)?);
        }
        Ok(stmt)
    }

    /// Number of records matching `domain`, computed in the database.
    pub async fn count<C: ConnectionTrait>(
        &self,
        db: &C,
        domain: Option<&JsonValue>,
    ) -> Result<u64> {
        let stmt = self.count_statement(domain, db.get_database_backend())?;
        let row = db.query_one(&stmt).await?;
        let count: i64 = match row {
            Some(row) => row.try_get("", "count")?,
            None => 0,
        };
        Ok(u64::try_from(count).unwrap_or_default())
    }

    /// Build the grouped `SELECT` that [`read_group`](Self::read_group) runs.
    pub fn read_group_statement(
        &self,
        params: &ReadGroupParams,
        backend: DbBackend,
    ) -> Result<SelectStatement> {
        let (query, pagination) = params.parse()?;
        compile_read_group(&self.compiler(backend), &query, &self.schema, &pagination)
    }

    /// Grouped aggregation. Each group carries a `__domain` that selects its records.
    pub async fn read_group<C: ConnectionTrait>(
        &self,
        db: &C,
        params: &ReadGroupParams,
    ) -> Result<Vec<Group>> {
        let (query, _) = params.parse()?;
        let stmt = self.read_group_statement(params, db.get_database_backend())?;
        let rows = db.query_all(&stmt).await?;
        rows.iter()
            .map(|row| {
                let JsonValue::Object(mut group) = JsonValue::from_query_result(row, "")? else {
                    unreachable!("JSON rows decode to objects");
                };
                for alias in query.aliases() {
                    if !group.contains_key(&alias) {
                        group.insert(alias.clone(), decode_untyped(row, &alias)?);
                    }
                }
                let domain = build_group_domain(&group, &query.groupby, params.domain.as_ref());
                group.insert("__domain".to_owned(), domain);
                Ok(group)
            })
            .collect()
    }
}

/// Decode a computed column whose type the driver does not report.
///
/// SQLite declares no type for expressions such as `COUNT(*)` or `strftime(...)`,
/// so SeaORM's JSON row decoder skips them; try the plausible types in turn.
fn decode_untyped(row: &QueryResult, alias: &str) -> Result<JsonValue> {
    fn attempt<T: TryGetable + Into<JsonValue>>(
        row: &QueryResult,
        alias: &str,
    ) -> Option<JsonValue> {
        row.try_get::<Option<T>>("", alias)
            .ok()
            .map(|v| v.map_or(JsonValue::Null, Into::into))
    }
    attempt::<i64>(row, alias)
        .or_else(|| attempt::<f64>(row, alias))
        .or_else(|| attempt::<String>(row, alias))
        .or_else(|| {
            row.try_get::<Option<Decimal>>("", alias)
                .ok()
                .map(|v| v.map_or(JsonValue::Null, |d| JsonValue::String(d.to_string())))
        })
        .or_else(|| attempt::<bool>(row, alias))
        .ok_or_else(|| QueryError::Db(DbErr::Type(format!("Cannot decode column '{alias}'"))))
}
