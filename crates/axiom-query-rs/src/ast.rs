//! Immutable AST nodes for the query specification DSL.

use std::ops::{BitAnd, BitOr, Not};

use serde_json::Value as JsonValue;

use crate::op::Op;

/// Leaf node: `field_path <operator> value`.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub field_path: String,
    pub op: Op,
    pub value: JsonValue,
}

/// A query specification: a boolean tree of conditions.
///
/// Compose with `&`, `|` and `!`:
///
/// ```
/// use axiom_query_rs::{Op, QuerySpec};
///
/// let spec = QuerySpec::condition("status", Op::Eq, "CONFIRMED")
///     & !QuerySpec::condition("total", Op::Eq, 0);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum QuerySpec {
    Condition(Condition),
    And(Box<QuerySpec>, Box<QuerySpec>),
    Or(Box<QuerySpec>, Box<QuerySpec>),
    Not(Box<QuerySpec>),
    Bool(bool),
}

impl QuerySpec {
    pub fn condition(field_path: impl Into<String>, op: Op, value: impl Into<JsonValue>) -> Self {
        Self::Condition(Condition {
            field_path: field_path.into(),
            op,
            value: value.into(),
        })
    }

    pub fn and(self, other: QuerySpec) -> Self {
        Self::And(Box::new(self), Box::new(other))
    }

    pub fn or(self, other: QuerySpec) -> Self {
        Self::Or(Box::new(self), Box::new(other))
    }

    pub fn negate(self) -> Self {
        Self::Not(Box::new(self))
    }
}

impl From<Condition> for QuerySpec {
    fn from(condition: Condition) -> Self {
        Self::Condition(condition)
    }
}

impl BitAnd for QuerySpec {
    type Output = QuerySpec;

    fn bitand(self, rhs: QuerySpec) -> QuerySpec {
        self.and(rhs)
    }
}

impl BitOr for QuerySpec {
    type Output = QuerySpec;

    fn bitor(self, rhs: QuerySpec) -> QuerySpec {
        self.or(rhs)
    }
}

impl Not for QuerySpec {
    type Output = QuerySpec;

    fn not(self) -> QuerySpec {
        self.negate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operators_build_tree() {
        let a = QuerySpec::condition("a", Op::Eq, 1);
        let b = QuerySpec::condition("b", Op::Eq, 2);
        assert_eq!(
            a.clone() & !b.clone(),
            QuerySpec::And(
                Box::new(a.clone()),
                Box::new(QuerySpec::Not(Box::new(b.clone())))
            )
        );
        assert_eq!(
            a.clone() | b.clone(),
            QuerySpec::Or(Box::new(a), Box::new(b))
        );
    }
}
