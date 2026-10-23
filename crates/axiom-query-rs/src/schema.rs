//! Table, column and relation metadata derived from SeaORM entities.
//!
//! Rust has no runtime reflection, so the schema is read from the traits the
//! SeaORM derive macros generate: `Column::iter()` / `ColumnTrait::def()` for
//! columns, and `Relation::iter()` / `RelationTrait::def()` for relations.

use std::collections::HashMap;
use std::sync::Arc;

use sea_orm::sea_query::{ColumnType, DynIden, IntoIden, TableName, TableRef};
use sea_orm::{
    ColumnTrait, EntityTrait, IdenStatic, Iterable, RelationDef, RelationTrait, RelationType,
};

/// A scalar column on an entity.
#[derive(Debug, Clone)]
pub struct ColumnInfo {
    /// Column name, as used in domain field paths.
    pub name: &'static str,
    pub column_type: ColumnType,
    pub nullable: bool,
    /// Database enum type name (Postgres `ENUM`), if any.
    pub enum_type_name: Option<&'static str>,
}

impl ColumnInfo {
    pub fn is_numeric(&self) -> bool {
        matches!(
            self.column_type,
            ColumnType::TinyInteger
                | ColumnType::SmallInteger
                | ColumnType::Integer
                | ColumnType::BigInteger
                | ColumnType::TinyUnsigned
                | ColumnType::SmallUnsigned
                | ColumnType::Unsigned
                | ColumnType::BigUnsigned
                | ColumnType::Float
                | ColumnType::Double
                | ColumnType::Decimal(_)
                | ColumnType::Money(_)
        )
    }

    pub fn is_temporal(&self) -> bool {
        matches!(
            self.column_type,
            ColumnType::Date
                | ColumnType::DateTime
                | ColumnType::Timestamp
                | ColumnType::TimestampWithTimeZone
        )
    }
}

/// Cardinality of a relation, seen from the entity that declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    /// `has_many`: many rows on the other side (fans out in joins).
    OneToMany,
    /// `belongs_to`: this table holds the foreign key.
    ManyToOne,
    /// `has_one`: at most one row on the other side.
    OneToOne,
}

/// A relation on an entity, traversable in dot-notation field paths.
#[derive(Debug, Clone)]
pub struct RelationInfo {
    /// Path segment: the `Relation` enum variant in snake_case (`Lines` → `lines`).
    pub name: String,
    pub kind: RelationKind,
    /// Registry key of the related entity's table.
    pub target: String,
    /// The SeaORM relation, including any extra `on_condition`.
    pub(crate) def: RelationDef,
}

/// Metadata for one entity.
#[derive(Debug, Clone)]
pub struct ModelSchema {
    /// Registry key: `schema.table` or `table`.
    pub key: String,
    pub table: &'static str,
    pub columns: Vec<ColumnInfo>,
    pub relations: Vec<RelationInfo>,
    pub(crate) table_ref: TableRef,
}

impl ModelSchema {
    /// Derive the schema of entity `E`.
    pub fn of<E: EntityTrait>() -> Self {
        let entity = E::default();
        let table_ref = entity.table_ref();

        let columns = E::Column::iter()
            .map(|col| {
                let def = col.def();
                ColumnInfo {
                    name: col.as_str(),
                    column_type: def.get_column_type().clone(),
                    nullable: def.is_null(),
                    enum_type_name: col.enum_type_name(),
                }
            })
            .collect();

        let relations = E::Relation::iter()
            .map(|rel| {
                let def = rel.def();
                let kind = match (&def.rel_type, def.is_owner) {
                    (RelationType::HasMany, _) => RelationKind::OneToMany,
                    (RelationType::HasOne, false) => RelationKind::ManyToOne,
                    (RelationType::HasOne, true) => RelationKind::OneToOne,
                };
                RelationInfo {
                    name: snake_case(&rel.name()),
                    kind,
                    target: table_key(&def.to_tbl),
                    def,
                }
            })
            .collect();

        Self {
            key: table_key(&table_ref),
            table: entity.table_name(),
            columns,
            relations,
            table_ref,
        }
    }

    pub fn column(&self, name: &str) -> Option<&ColumnInfo> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn relation(&self, name: &str) -> Option<&RelationInfo> {
        self.relations.iter().find(|r| r.name == name)
    }

    pub(crate) fn table_iden(&self) -> DynIden {
        self.table.into_iden()
    }

    pub(crate) fn column_names(&self) -> String {
        self.columns
            .iter()
            .map(|c| c.name)
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub(crate) fn relation_names(&self) -> String {
        if self.relations.is_empty() {
            return "none".to_owned();
        }
        self.relations
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Every entity reachable through relation paths, keyed by table.
///
/// Register each entity that a field path may traverse into; the engine's
/// own entity is always available.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    schemas: HashMap<String, Arc<ModelSchema>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<E: EntityTrait>(mut self) -> Self {
        self.add::<E>();
        self
    }

    pub fn add<E: EntityTrait>(&mut self) -> &mut Self {
        let schema = ModelSchema::of::<E>();
        self.schemas.insert(schema.key.clone(), Arc::new(schema));
        self
    }

    pub fn get(&self, key: &str) -> Option<&Arc<ModelSchema>> {
        self.schemas.get(key)
    }
}

fn table_key(table_ref: &TableRef) -> String {
    match table_ref {
        TableRef::Table(TableName(Some(schema), table), _) => {
            format!("{}.{}", schema.1.inner(), table.inner())
        }
        TableRef::Table(TableName(None, table), _) => table.inner().into_owned(),
        other => other.sea_orm_table().inner().into_owned(),
    }
}

fn snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::snake_case;

    #[test]
    fn relation_names_are_snake_case() {
        assert_eq!(snake_case("Lines"), "lines");
        assert_eq!(snake_case("OrderLine"), "order_line");
        assert_eq!(snake_case("customer"), "customer");
    }
}
