//! Relation paths use the relation's full join condition, not just its foreign key.

mod common;

use std::sync::Arc;

use axiom_query_rs::{ListParams, QueryEngine, ReadGroupParams, Registry};
use sea_orm::ConnectionTrait;
use serde_json::json;

mod message {
    use sea_orm::entity::prelude::*;

    /// Points to any record by model name + id (no real foreign key).
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "message")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub model: String,
        pub res_id: i32,
        pub body: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl Related<super::ticket::Entity> for Entity {
        fn to() -> RelationDef {
            Entity::belongs_to(super::ticket::Entity)
                .from(Column::ResId)
                .to(super::ticket::Column::Id)
                .into()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

mod ticket {
    use sea_orm::entity::prelude::*;
    use sea_orm::sea_query::{ExprTrait, IntoCondition};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "ticket")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {
        Messages,
    }

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            match self {
                Self::Messages => Entity::has_many(super::message::Entity)
                    .on_condition(|_ticket, message| {
                        Expr::col((message, super::message::Column::Model))
                            .eq("ticket")
                            .into_condition()
                    })
                    .into(),
            }
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

async fn setup() -> (sea_orm::DatabaseConnection, QueryEngine<ticket::Entity>) {
    let db = common::fresh_db().await;
    common::create_tables(&db, ticket::Entity).await;
    common::create_tables(&db, message::Entity).await;
    db.execute_unprepared(
        "INSERT INTO ticket (id) VALUES (1), (2);
         INSERT INTO message (model, res_id, body) VALUES ('invoice', 1, 'paid'), ('ticket', 2, 'urgent');",
    )
    .await
    .unwrap();
    let registry = Arc::new(Registry::new().register::<message::Entity>());
    (db, QueryEngine::new(registry))
}

#[tokio::test]
async fn extra_join_condition_is_applied() {
    let (db, engine) = setup().await;
    let paid = engine
        .list(
            &db,
            &ListParams::new().domain(json!([["messages.body", "=", "paid"]])),
        )
        .await
        .unwrap();
    assert!(paid.is_empty());
    let urgent = engine
        .list(
            &db,
            &ListParams::new().domain(json!([["messages.body", "=", "urgent"]])),
        )
        .await
        .unwrap();
    assert_eq!(urgent.iter().map(|t| t.id).collect::<Vec<_>>(), [2]);
}

#[tokio::test]
async fn count_and_read_group_use_the_full_join_condition() {
    let (db, engine) = setup().await;
    assert_eq!(
        engine
            .count(&db, Some(&json!([["messages.body", "=", "paid"]])))
            .await
            .unwrap(),
        0
    );

    let groups = engine
        .read_group(&db, &ReadGroupParams::new(["id"], ["messages.id:count"]))
        .await
        .unwrap();
    let mut counts: Vec<_> = groups
        .iter()
        .map(|g| (g["id"].clone(), g["messages__id__count"].clone()))
        .collect();
    counts.sort_by_key(|(id, _)| id.as_i64());
    assert_eq!(counts, [(json!(1), json!(0)), (json!(2), json!(1))]);
}

#[tokio::test]
async fn read_group_joins_use_the_full_join_condition() {
    let (db, engine) = setup().await;
    let groups = engine
        .read_group(&db, &ReadGroupParams::new(["messages.body"], ["__count"]))
        .await
        .unwrap();
    // Ticket 1 has no messages of its own; the invoice's "paid" message must not join to it.
    let mut counts: Vec<_> = groups
        .iter()
        .map(|g| (g["messages__body"].clone(), g["__count"].clone()))
        .collect();
    counts.sort_by_key(|(body, _)| !body.is_null());
    assert_eq!(
        counts,
        [(json!(null), json!(1)), (json!("urgent"), json!(1))]
    );
}
