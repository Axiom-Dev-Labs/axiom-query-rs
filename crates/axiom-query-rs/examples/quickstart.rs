//! Filter, count, stream and group orders with JSON domains.
//!
//! Run with: `cargo run --example quickstart`

use std::sync::Arc;

use axiom_query_rs::{ListParams, OrderBy, QueryEngine, ReadGroupParams, Registry};
use futures_util::TryStreamExt;
use sea_orm::{ConnectionTrait, Database, Schema};
use serde_json::json;

mod customer {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "customers")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod order {
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

mod order_line {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "order_lines")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub order_id: i32,
        pub quantity: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::order::Entity",
            from = "Column::OrderId",
            to = "super::order::Column::Id"
        )]
        Order,
    }

    impl Related<super::order::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Order.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = Database::connect("sqlite::memory:").await?;
    let schema = Schema::new(db.get_database_backend());
    db.execute(&schema.create_table_from_entity(customer::Entity))
        .await?;
    db.execute(&schema.create_table_from_entity(order::Entity))
        .await?;
    db.execute(&schema.create_table_from_entity(order_line::Entity))
        .await?;
    db.execute_unprepared(
        "INSERT INTO customers (id, name) VALUES (1, 'Alice'), (2, 'Bob');
         INSERT INTO orders (id, status, total, created_at, customer_id) VALUES
             (1, 'CONFIRMED', 100, '2026-01-15 00:00:00', 1),
             (2, 'CONFIRMED', 200, '2026-02-20 00:00:00', 2),
             (3, 'DRAFT', 50, '2026-01-25 00:00:00', NULL);
         INSERT INTO order_lines (order_id, quantity) VALUES (1, 2), (1, 3), (2, 1);",
    )
    .await?;

    // Register every entity a dot path may reach; the engine's own entity is implicit.
    let registry = Arc::new(
        Registry::new()
            .register::<customer::Entity>()
            .register::<order_line::Entity>(),
    );
    let engine = QueryEngine::<order::Entity>::new(registry);

    // list: filter, order and paginate
    let params = ListParams::new()
        .domain(json!([
            ["status", "=", "CONFIRMED"],
            ["customer.name", "istarts_with", "al"],
        ]))
        .order_by(OrderBy::desc("total"))
        .limit(20);
    let orders = engine.list(&db, &params).await?;
    println!(
        "Alice's confirmed orders: {:?}",
        orders.iter().map(|o| o.id).collect::<Vec<_>>()
    );

    // count: SELECT COUNT(*) with the same domain language
    let big = engine
        .count(&db, Some(&json!([["lines.quantity", ">", 2]])))
        .await?;
    println!("Orders with a line of quantity > 2: {big}");

    // search: stream every match
    let mut stream = engine.search(&db, None, &[OrderBy::asc("id")]).await?;
    while let Some(order) = stream.try_next().await? {
        println!("  streamed order {} ({})", order.id, order.status);
    }
    // The stream holds a pooled connection until dropped.
    drop(stream);

    // read_group: GROUP BY with aggregates, HAVING and a drill-down domain per group
    let groups = engine
        .read_group(
            &db,
            &ReadGroupParams::new(["created_at:month"], ["__count", "total:sum"])
                .order_by(OrderBy::asc("created_at__month")),
        )
        .await?;
    for group in &groups {
        let records = engine
            .list(&db, &ListParams::new().domain(group["__domain"].clone()))
            .await?;
        println!(
            "{}: {} orders, total {} (drill-down found {})",
            group["created_at__month"],
            group["__count"],
            group["total__sum"],
            records.len()
        );
    }
    Ok(())
}
