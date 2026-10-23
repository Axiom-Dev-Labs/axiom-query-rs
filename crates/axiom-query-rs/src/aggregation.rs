//! `read_group` (GROUP BY + aggregation) specifications and their parser.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::ast::QuerySpec;
use crate::error::{ErrorCode, QueryError, Result};
use crate::parser::{json_type_name, parse_domain};

macro_rules! str_enum {
    ($(#[$meta:meta])* $name:ident, $what:literal { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub const fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = QueryError;

            fn from_str(s: &str) -> Result<Self> {
                let lower = s.to_lowercase();
                Self::ALL.iter().copied().find(|v| v.as_str() == lower).ok_or_else(|| {
                    let valid = Self::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ");
                    QueryError::new(
                        ErrorCode::InvalidAggregation,
                        format!(concat!("Unknown ", $what, " '{}'. Valid: {}"), s, valid),
                    )
                })
            }
        }
    };
}

str_enum!(
    /// Supported SQL aggregate functions.
    AggFunc, "aggregate function" {
        Count => "count",
        Sum => "sum",
        Avg => "avg",
        Min => "min",
        Max => "max",
    }
);

str_enum!(
    /// Date truncation granularities for GROUP BY on date/datetime fields.
    DateGranularity, "date granularity" {
        Day => "day",
        Week => "week",
        Month => "month",
        Quarter => "quarter",
        Year => "year",
    }
);

/// One aggregate expression in the SELECT list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateSpec {
    /// `None` for `__count` (COUNT(*)).
    pub field_path: Option<String>,
    pub function: AggFunc,
    pub alias: String,
}

/// One GROUP BY expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupBySpec {
    pub field_path: String,
    pub granularity: Option<DateGranularity>,
}

impl GroupBySpec {
    /// Result key: dots become `__`, plus `__<granularity>` when truncated.
    pub fn alias(&self) -> String {
        let base = self.field_path.replace('.', "__");
        match self.granularity {
            Some(g) => format!("{base}__{g}"),
            None => base,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Asc,
    Desc,
}

/// An ORDER BY item: `"field"` or `["field", "asc"|"desc"]` in JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "JsonValue", into = "JsonValue")]
pub struct OrderBy {
    pub field: String,
    pub direction: Direction,
}

impl OrderBy {
    pub fn asc(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            direction: Direction::Asc,
        }
    }

    pub fn desc(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            direction: Direction::Desc,
        }
    }
}

impl TryFrom<JsonValue> for OrderBy {
    type Error = QueryError;

    fn try_from(value: JsonValue) -> Result<Self> {
        let invalid = |message: String| QueryError::new(ErrorCode::InvalidOrderBy, message);
        match &value {
            JsonValue::String(field) => Ok(Self::asc(field.clone())),
            JsonValue::Array(pair) if pair.len() == 2 => {
                let field = pair[0].as_str().ok_or_else(|| {
                    invalid(format!(
                        "order_by field must be a string, got {}",
                        json_type_name(&pair[0])
                    ))
                })?;
                let direction = match pair[1].as_str().map(str::to_lowercase).as_deref() {
                    Some("asc") => Direction::Asc,
                    Some("desc") => Direction::Desc,
                    _ => {
                        return Err(invalid(format!(
                            "order_by direction must be 'asc' or 'desc', got {}",
                            pair[1]
                        )));
                    }
                };
                Ok(Self {
                    field: field.to_owned(),
                    direction,
                })
            }
            _ => Err(invalid(
                "order_by items must be 'field' or ['field', 'asc'|'desc']".to_owned(),
            )),
        }
    }
}

impl From<OrderBy> for JsonValue {
    fn from(order: OrderBy) -> Self {
        let direction = match order.direction {
            Direction::Asc => "asc",
            Direction::Desc => "desc",
        };
        JsonValue::Array(vec![order.field.into(), direction.into()])
    }
}

/// Parse a JSON `order_by` list.
pub fn parse_order_by(raw: &JsonValue) -> Result<Vec<OrderBy>> {
    match raw {
        JsonValue::Null => Ok(Vec::new()),
        JsonValue::Array(items) => items.iter().cloned().map(OrderBy::try_from).collect(),
        _ => Err(QueryError::new(
            ErrorCode::InvalidOrderBy,
            "order_by must be a list of 'field' or ['field', direction] items",
        )),
    }
}

/// Presentation concerns for grouped queries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pagination {
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// A complete, validated `read_group` query.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadGroupQuery {
    pub groupby: Vec<GroupBySpec>,
    pub aggregates: Vec<AggregateSpec>,
    pub domain: Option<QuerySpec>,
    pub having: Option<QuerySpec>,
}

impl ReadGroupQuery {
    /// Result keys, groupby aliases first.
    pub fn aliases(&self) -> Vec<String> {
        self.groupby
            .iter()
            .map(GroupBySpec::alias)
            .chain(self.aggregates.iter().map(|a| a.alias.clone()))
            .collect()
    }

    /// Distinct relation-path prefixes across groupby and aggregate field paths.
    /// `"lines.product.category"` contributes `["lines", "product"]`.
    pub fn referenced_relation_paths(&self) -> BTreeSet<Vec<String>> {
        self.groupby
            .iter()
            .map(|g| g.field_path.as_str())
            .chain(
                self.aggregates
                    .iter()
                    .filter_map(|a| a.field_path.as_deref()),
            )
            .filter_map(|path| {
                let (prefix, _) = path.rsplit_once('.')?;
                Some(prefix.split('.').map(str::to_owned).collect())
            })
            .collect()
    }
}

/// `read_group` request, deserializable from a JSON body.
///
/// ```
/// use axiom_query_rs::{OrderBy, ReadGroupParams};
/// use serde_json::json;
///
/// let params = ReadGroupParams::new(["status", "created_at:month"], ["__count", "total:sum"])
///     .domain(json!([["total", ">", 0]]))
///     .having(json!([["__count", ">", 1]]))
///     .order_by(OrderBy::desc("total__sum"))
///     .limit(10);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReadGroupParams {
    /// `"field"` or `"field:granularity"`; dot paths traverse relations.
    pub groupby: Vec<String>,
    /// `"__count"` or `"field:function"`.
    pub aggregates: Vec<String>,
    pub domain: Option<JsonValue>,
    /// Filter on groupby / aggregate aliases.
    pub having: Option<JsonValue>,
    /// Ordered by groupby / aggregate aliases.
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

impl Default for ReadGroupParams {
    fn default() -> Self {
        Self {
            groupby: Vec::new(),
            aggregates: vec!["__count".to_owned()],
            domain: None,
            having: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }
    }
}

impl ReadGroupParams {
    pub fn new<G, A>(groupby: G, aggregates: A) -> Self
    where
        G: IntoIterator,
        G::Item: Into<String>,
        A: IntoIterator,
        A::Item: Into<String>,
    {
        Self {
            groupby: groupby.into_iter().map(Into::into).collect(),
            aggregates: aggregates.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    pub fn domain(mut self, domain: JsonValue) -> Self {
        self.domain = Some(domain);
        self
    }

    pub fn having(mut self, having: JsonValue) -> Self {
        self.having = Some(having);
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

    /// Validate and parse into a query and its pagination.
    pub fn parse(&self) -> Result<(ReadGroupQuery, Pagination)> {
        if self.aggregates.is_empty() {
            return Err(QueryError::new(
                ErrorCode::InvalidReadGroup,
                "aggregates must contain at least one spec (e.g. '__count')",
            ));
        }
        let query = ReadGroupQuery {
            groupby: self
                .groupby
                .iter()
                .map(|s| parse_groupby_spec(s))
                .collect::<Result<_>>()?,
            aggregates: self
                .aggregates
                .iter()
                .map(|s| parse_aggregate_spec(s))
                .collect::<Result<_>>()?,
            domain: self.domain.as_ref().map(parse_domain).transpose()?,
            having: self.having.as_ref().map(parse_domain).transpose()?,
        };
        let pagination = Pagination {
            order_by: self.order_by.clone(),
            limit: self.limit,
            offset: self.offset,
        };
        Ok((query, pagination))
    }
}

/// Parse a JSON `read_group` request body.
pub fn parse_read_group(raw: &JsonValue) -> Result<(ReadGroupQuery, Pagination)> {
    if !raw.is_object() {
        return Err(QueryError::new(
            ErrorCode::InvalidReadGroup,
            format!(
                "read_group body must be a dict, got {}",
                json_type_name(raw)
            ),
        ));
    }
    let params: ReadGroupParams = serde_json::from_value(raw.clone())
        .map_err(|err| QueryError::new(ErrorCode::InvalidReadGroup, err.to_string()))?;
    params.parse()
}

/// Parse `"__count"` or `"field:function"`.
pub fn parse_aggregate_spec(spec: &str) -> Result<AggregateSpec> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(QueryError::new(
            ErrorCode::InvalidAggregation,
            "Aggregate spec must be a non-empty string",
        ));
    }
    if spec == "__count" {
        return Ok(AggregateSpec {
            field_path: None,
            function: AggFunc::Count,
            alias: "__count".to_owned(),
        });
    }
    let (field_path, func) = match spec.rsplit_once(':') {
        Some((field, func)) if !field.is_empty() && !func.is_empty() => (field, func),
        Some(_) => {
            return Err(QueryError::new(
                ErrorCode::InvalidAggregation,
                format!("Invalid aggregate spec format: '{spec}'. Expected 'field:function'."),
            ));
        }
        None => {
            return Err(QueryError::new(
                ErrorCode::InvalidAggregation,
                format!("Aggregate spec must be 'field:function' or '__count', got '{spec}'"),
            ));
        }
    };
    let function = func.parse::<AggFunc>()?;
    Ok(AggregateSpec {
        field_path: Some(field_path.to_owned()),
        function,
        alias: format!("{}__{function}", field_path.replace('.', "__")),
    })
}

/// Parse `"field"` or `"field:granularity"`.
pub fn parse_groupby_spec(spec: &str) -> Result<GroupBySpec> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(QueryError::new(
            ErrorCode::InvalidAggregation,
            "Groupby spec must be a non-empty string",
        ));
    }
    match spec.rsplit_once(':') {
        None => Ok(GroupBySpec {
            field_path: spec.to_owned(),
            granularity: None,
        }),
        Some((field, gran)) if !field.is_empty() && !gran.is_empty() => Ok(GroupBySpec {
            field_path: field.to_owned(),
            granularity: Some(gran.parse()?),
        }),
        Some(_) => Err(QueryError::new(
            ErrorCode::InvalidAggregation,
            format!("Invalid groupby spec format: '{spec}'. Expected 'field:granularity'."),
        )),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_aggregate_specs() {
        let count = parse_aggregate_spec("__count").unwrap();
        assert_eq!((count.field_path, count.alias.as_str()), (None, "__count"));

        let sum = parse_aggregate_spec("lines.quantity:SUM").unwrap();
        assert_eq!(sum.field_path.as_deref(), Some("lines.quantity"));
        assert_eq!(sum.function, AggFunc::Sum);
        assert_eq!(sum.alias, "lines__quantity__sum");

        for bad in ["", "total", "total:", ":sum", "total:median"] {
            assert_eq!(
                parse_aggregate_spec(bad).unwrap_err().code(),
                ErrorCode::InvalidAggregation
            );
        }
    }

    #[test]
    fn parses_groupby_specs() {
        let g = parse_groupby_spec("created_at:month").unwrap();
        assert_eq!(g.granularity, Some(DateGranularity::Month));
        assert_eq!(g.alias(), "created_at__month");
        assert_eq!(
            parse_groupby_spec("customer.city.name").unwrap().alias(),
            "customer__city__name"
        );
        assert!(parse_groupby_spec("created_at:fortnight").is_err());
    }

    #[test]
    fn parses_read_group_body() {
        let (query, pagination) = parse_read_group(&json!({
            "groupby": ["status", "lines.product.category"],
            "aggregates": ["__count", "lines.quantity:sum"],
            "domain": [["total", ">", 0]],
            "having": [["__count", ">", 1]],
            "order_by": ["status", ["__count", "DESC"]],
            "limit": 5,
        }))
        .unwrap();
        assert_eq!(query.groupby.len(), 2);
        assert!(query.domain.is_some() && query.having.is_some());
        assert_eq!(
            pagination.order_by,
            vec![OrderBy::asc("status"), OrderBy::desc("__count")]
        );
        assert_eq!(pagination.limit, Some(5));
        assert_eq!(
            query
                .referenced_relation_paths()
                .into_iter()
                .collect::<Vec<_>>(),
            vec![
                vec!["lines".to_owned()],
                vec!["lines".to_owned(), "product".to_owned()]
            ]
        );
    }

    #[test]
    fn aggregates_default_to_count() {
        let (query, _) = parse_read_group(&json!({"groupby": ["status"]})).unwrap();
        assert_eq!(query.aggregates[0].alias, "__count");
    }

    #[test]
    fn rejects_invalid_bodies() {
        for raw in [
            json!([]),
            json!({"aggregates": []}),
            json!({"limit": -1}),
            json!({"order_by": [["a", "sideways"]]}),
            json!({"unknown": 1}),
        ] {
            assert_eq!(
                parse_read_group(&raw).unwrap_err().code(),
                ErrorCode::InvalidReadGroup,
                "{raw}"
            );
        }
    }
}
