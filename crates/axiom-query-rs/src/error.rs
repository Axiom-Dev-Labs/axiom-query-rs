//! Query error types.

use std::fmt;

use sea_orm::DbErr;

/// Machine-readable error code, stable across releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    InvalidDomain,
    InvalidFilterField,
    InvalidFilterValue,
    InvalidHavingField,
    InvalidHavingOperator,
    InvalidAggregation,
    InvalidReadGroup,
    InvalidOrderBy,
    MultiChildAggregation,
    UnregisteredEntity,
    Database,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidDomain => "INVALID_DOMAIN",
            Self::InvalidFilterField => "INVALID_FILTER_FIELD",
            Self::InvalidFilterValue => "INVALID_FILTER_VALUE",
            Self::InvalidHavingField => "INVALID_HAVING_FIELD",
            Self::InvalidHavingOperator => "INVALID_HAVING_OPERATOR",
            Self::InvalidAggregation => "INVALID_AGGREGATION",
            Self::InvalidReadGroup => "INVALID_READ_GROUP",
            Self::InvalidOrderBy => "INVALID_ORDER_BY",
            Self::MultiChildAggregation => "MULTI_CHILD_AGGREGATION",
            Self::UnregisteredEntity => "UNREGISTERED_ENTITY",
            Self::Database => "DATABASE_ERROR",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Raised when a query specification is invalid, or when the database fails.
///
/// Specification errors are detected before any SQL reaches the database.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("{code}: {message}")]
    Invalid { code: ErrorCode, message: String },
    #[error(transparent)]
    Db(#[from] DbErr),
}

impl QueryError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Invalid {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Invalid { code, .. } => *code,
            Self::Db(_) => ErrorCode::Database,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Invalid { message, .. } => message.clone(),
            Self::Db(err) => err.to_string(),
        }
    }
}

pub type Result<T, E = QueryError> = std::result::Result<T, E>;
