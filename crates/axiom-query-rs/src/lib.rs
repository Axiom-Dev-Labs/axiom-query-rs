//! Specification-based query and aggregation engine for SeaORM entities.
//!
//! Filters are composable data — JSON domains such as
//! `[["status", "=", "CONFIRMED"], ["lines.quantity", ">", 2]]` or [`QuerySpec`]
//! trees — compiled to SQL against any registered entity.

pub mod aggregation;
pub mod ast;
mod compiler;
mod compiler_aggregate;
pub mod engine;
pub mod error;
pub mod op;
pub mod parser;
pub mod schema;
mod value;

pub use aggregation::{
    AggFunc, AggregateSpec, DateGranularity, Direction, GroupBySpec, OrderBy, Pagination,
    ReadGroupParams, ReadGroupQuery, parse_order_by, parse_read_group,
};
pub use ast::{Condition, QuerySpec};
pub use compiler_aggregate::build_group_domain;
pub use engine::{Group, ListParams, QueryEngine};
pub use error::{ErrorCode, QueryError, Result};
pub use op::Op;
pub use parser::parse_domain;
pub use schema::{ColumnInfo, ModelSchema, Registry, RelationInfo, RelationKind};
