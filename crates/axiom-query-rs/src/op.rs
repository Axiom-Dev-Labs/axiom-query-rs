//! Comparison operators for the specification DSL.

use std::fmt;
use std::str::FromStr;

use crate::error::{ErrorCode, QueryError};

/// Comparison operators supported in query specifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Lt,
    Gte,
    Lte,
    In,
    NotIn,
    Like,
    Ilike,
    IsNull,
    Contains,
    IContains,
    StartsWith,
    IStartsWith,
    EndsWith,
    IEndsWith,
}

impl Op {
    pub const ALL: [Op; 17] = [
        Op::Eq,
        Op::Ne,
        Op::Gt,
        Op::Lt,
        Op::Gte,
        Op::Lte,
        Op::In,
        Op::NotIn,
        Op::Like,
        Op::Ilike,
        Op::IsNull,
        Op::Contains,
        Op::IContains,
        Op::StartsWith,
        Op::IStartsWith,
        Op::EndsWith,
        Op::IEndsWith,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "!=",
            Op::Gt => ">",
            Op::Lt => "<",
            Op::Gte => ">=",
            Op::Lte => "<=",
            Op::In => "in",
            Op::NotIn => "not in",
            Op::Like => "like",
            Op::Ilike => "ilike",
            Op::IsNull => "is_null",
            Op::Contains => "contains",
            Op::IContains => "icontains",
            Op::StartsWith => "starts_with",
            Op::IStartsWith => "istarts_with",
            Op::EndsWith => "ends_with",
            Op::IEndsWith => "iends_with",
        }
    }

    /// Text-match operators, compiled to `LIKE` / `ILIKE`.
    pub const fn is_text_match(self) -> bool {
        matches!(
            self,
            Op::Like
                | Op::Ilike
                | Op::Contains
                | Op::IContains
                | Op::StartsWith
                | Op::IStartsWith
                | Op::EndsWith
                | Op::IEndsWith
        )
    }

    pub const fn is_case_insensitive(self) -> bool {
        matches!(
            self,
            Op::Ilike | Op::IContains | Op::IStartsWith | Op::IEndsWith
        )
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Op {
    type Err = QueryError;

    /// Parse an operator string. Case-insensitive, surrounding whitespace ignored.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_lowercase();
        Op::ALL
            .into_iter()
            .find(|op| op.as_str() == normalized)
            .ok_or_else(|| {
                QueryError::new(ErrorCode::InvalidDomain, format!("Unknown operator: {s:?}"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_case_insensitively() {
        assert_eq!(" NOT IN ".parse::<Op>().unwrap(), Op::NotIn);
        assert_eq!("ILike".parse::<Op>().unwrap(), Op::Ilike);
        assert_eq!("is_null".parse::<Op>().unwrap(), Op::IsNull);
        assert_eq!("IStarts_With".parse::<Op>().unwrap(), Op::IStartsWith);
    }

    #[test]
    fn rejects_unknown_operator() {
        let err = "~=".parse::<Op>().unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidDomain);
    }
}
