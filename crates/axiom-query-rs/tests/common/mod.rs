//! Shared fixtures: the AxiomQuery test schema as SeaORM entities.
//!
//! Tests run on in-memory SQLite by default. Set `AXIOM_TEST_DATABASE_URL` to a
//! Postgres URL to run them there instead; each test gets its own schema.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axiom_query_rs::{QueryEngine, Registry};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, EntityTrait, Schema};

pub mod country {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "countries")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod city {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "cities")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub name: String,
        pub country_id: Option<i32>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::country::Entity",
            from = "Column::CountryId",
            to = "super::country::Column::Id"
        )]
        Country,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod customer {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "customers")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub name: String,
        pub city_id: Option<i32>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::city::Entity",
            from = "Column::CityId",
            to = "super::city::Column::Id"
        )]
        City,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod product {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "products")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub name: String,
        pub category: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod order {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "orders")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub status: String,
        pub total: i32,
        pub created_at: DateTime,
        pub customer_id: Option<i32>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::customer::Entity",
            from = "Column::CustomerId",
            to = "super::customer::Column::Id"
        )]
        Customer,
        #[sea_orm(has_many = "super::order_line::Entity")]
        Lines,
    }

    impl Related<super::order_line::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Lines.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod order_line {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "order_lines")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub order_id: i32,
        pub product_name: String,
        pub quantity: i32,
        pub unit_price: i32,
        pub product_id: Option<i32>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::order::Entity",
            from = "Column::OrderId",
            to = "super::order::Column::Id"
        )]
        Order,
        #[sea_orm(
            belongs_to = "super::product::Entity",
            from = "Column::ProductId",
            to = "super::product::Column::Id"
        )]
        Product,
    }

    impl Related<super::order::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Order.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

const SEED: &str = "
    INSERT INTO countries (id, name) VALUES (1, 'India'), (2, 'USA');
    INSERT INTO cities (id, name, country_id) VALUES (1, 'Mumbai', 1), (2, 'New York', 2);
    INSERT INTO products (id, name, category) VALUES
        (1, 'Widget A', 'Hardware'), (2, 'Widget B', 'Hardware'), (3, 'Gadget', 'Electronics');
    INSERT INTO customers (id, name, city_id) VALUES (1, 'Alice', 1), (2, 'Bob', 2);
    INSERT INTO orders (id, status, total, created_at, customer_id) VALUES
        (1, 'CONFIRMED', 100, '2026-01-15 00:00:00', 1),
        (2, 'CONFIRMED', 200, '2026-02-20 00:00:00', 2),
        (3, 'DRAFT', 50, '2026-01-25 00:00:00', NULL);
    INSERT INTO order_lines (order_id, product_name, quantity, unit_price, product_id) VALUES
        (1, 'Widget A', 2, 50, 1),
        (1, 'Widget B', 3, 50, 2),
        (2, 'Gadget', 1, 200, 3);
";

pub async fn create_tables<E: EntityTrait>(db: &DatabaseConnection, entity: E) {
    let schema = Schema::new(db.get_database_backend());
    db.execute(&schema.create_table_from_entity(entity))
        .await
        .unwrap();
}

/// An empty, isolated database for one test.
pub async fn fresh_db() -> DatabaseConnection {
    match std::env::var("AXIOM_TEST_DATABASE_URL") {
        Ok(url) if url.starts_with("postgres") => postgres_schema(&url).await,
        Ok(url) => Database::connect(url).await.unwrap(),
        Err(_) => Database::connect("sqlite::memory:").await.unwrap(),
    }
}

/// Connect with `search_path` set to a schema private to this test.
///
/// Names are `aq_<test binary>_<n>`, recreated on each run, so reruns reuse
/// them instead of accumulating.
async fn postgres_schema(url: &str) -> DatabaseConnection {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let schema = format!(
        "aq_{}_{}",
        env!("CARGO_CRATE_NAME"),
        SEQ.fetch_add(1, Ordering::SeqCst)
    );

    let admin = Database::connect(url).await.unwrap();
    admin
        .execute_unprepared(&format!(
            r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE; CREATE SCHEMA "{schema}""#
        ))
        .await
        .unwrap();
    admin.close().await.unwrap();

    let mut options = ConnectOptions::new(url);
    options.set_schema_search_path(schema).max_connections(2);
    Database::connect(options).await.unwrap()
}

/// A fresh database with the seed data.
pub async fn seeded_db() -> DatabaseConnection {
    let db = fresh_db().await;
    create_tables(&db, country::Entity).await;
    create_tables(&db, city::Entity).await;
    create_tables(&db, customer::Entity).await;
    create_tables(&db, product::Entity).await;
    create_tables(&db, order::Entity).await;
    create_tables(&db, order_line::Entity).await;
    db.execute_unprepared(SEED).await.unwrap();
    db
}

pub fn registry() -> Arc<Registry> {
    Arc::new(
        Registry::new()
            .register::<country::Entity>()
            .register::<city::Entity>()
            .register::<customer::Entity>()
            .register::<product::Entity>()
            .register::<order::Entity>()
            .register::<order_line::Entity>(),
    )
}

pub fn engine() -> QueryEngine<order::Entity> {
    QueryEngine::new(registry())
}

pub async fn setup() -> (DatabaseConnection, QueryEngine<order::Entity>) {
    (seeded_db().await, engine())
}

pub fn ids(models: &[order::Model]) -> Vec<i32> {
    let mut ids: Vec<i32> = models.iter().map(|m| m.id).collect();
    ids.sort_unstable();
    ids
}
