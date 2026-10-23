//! Generated SQL per backend, checked without a database.

mod common;

use axiom_query_rs::ReadGroupParams;
use sea_orm::{DbBackend, QueryTrait};
use serde_json::json;

#[test]
fn relation_paths_compile_to_correlated_exists() {
    let engine = common::engine();
    let params =
        axiom_query_rs::ListParams::new().domain(json!([["customer.city.name", "ilike", "%mum%"]]));
    let sql = engine
        .select(&params, DbBackend::Postgres)
        .unwrap()
        .build(DbBackend::Postgres)
        .to_string();
    assert!(
        sql.ends_with(
            r#"WHERE EXISTS(SELECT 1 FROM "customers" AS "customers_1" WHERE "orders"."customer_id" = "customers_1"."id" AND EXISTS(SELECT 1 FROM "cities" AS "cities_2" WHERE "customers_1"."city_id" = "cities_2"."id" AND ("cities_2"."name" ILIKE '%mum%')))"#
        ),
        "{sql}"
    );

    let sql = engine
        .select(&params, DbBackend::Sqlite)
        .unwrap()
        .build(DbBackend::Sqlite)
        .to_string();
    assert!(
        sql.contains(r#"LOWER("cities_2"."name") LIKE '%mum%'"#),
        "{sql}"
    );
}

#[test]
fn postgres_read_group_inlines_the_granularity() {
    let engine = common::engine();
    let params = ReadGroupParams::new(
        ["created_at:month", "customer.name"],
        ["__count", "lines.quantity:sum"],
    )
    .having(json!([["__count", ">", 1]]));
    let stmt = engine
        .read_group_statement(&params, DbBackend::Postgres)
        .unwrap();
    let sql = DbBackend::Postgres.build(&stmt).to_string();
    assert_eq!(
        sql,
        concat!(
            r#"SELECT CAST(DATE_TRUNC('month', "orders"."created_at") AS DATE) AS "created_at__month", "#,
            r#""customers_1"."name" AS "customer__name", COUNT(*) AS "__count", "#,
            r#"SUM("order_lines_2"."quantity") AS "lines__quantity__sum" FROM "orders" "#,
            r#"LEFT JOIN "customers" AS "customers_1" ON "orders"."customer_id" = "customers_1"."id" "#,
            r#"LEFT JOIN "order_lines" AS "order_lines_2" ON "orders"."id" = "order_lines_2"."order_id" "#,
            r#"GROUP BY CAST(DATE_TRUNC('month', "orders"."created_at") AS DATE), "customers_1"."name" "#,
            r#"HAVING COUNT(*) > 1"#
        )
    );
}

#[test]
fn mysql_date_truncation() {
    let engine = common::engine();
    let stmt = engine
        .read_group_statement(
            &ReadGroupParams::new(["created_at:quarter"], ["__count"]),
            DbBackend::MySql,
        )
        .unwrap();
    let sql = DbBackend::MySql.build(&stmt).to_string();
    assert!(
        sql.starts_with(
            "SELECT MAKEDATE(YEAR(`orders`.`created_at`), 1) + INTERVAL (QUARTER(`orders`.`created_at`) - 1) QUARTER"
        ),
        "{sql}"
    );
}

#[test]
fn text_match_operators_escape_and_pick_like_or_ilike() {
    let engine = common::engine();
    let where_clause = |op: &str, value: &str, backend: DbBackend| {
        let params = axiom_query_rs::ListParams::new().domain(json!([["status", op, value]]));
        let sql = engine
            .select(&params, backend)
            .unwrap()
            .build(backend)
            .to_string();
        sql[sql.find("WHERE ").unwrap() + 6..].to_owned()
    };
    assert_eq!(
        where_clause("contains", "5%_a", DbBackend::Postgres),
        r#""orders"."status" LIKE E'%5\\%\\_a%'"#
    );
    assert_eq!(
        where_clause("istarts_with", "Dr", DbBackend::Postgres),
        r#""orders"."status" ILIKE 'Dr%'"#
    );
    assert_eq!(
        where_clause("iends_with", "Ed", DbBackend::Sqlite),
        r#"LOWER("orders"."status") LIKE '%ed' ESCAPE '\'"#
    );
    assert_eq!(
        where_clause("like", "5%", DbBackend::Postgres),
        r#""orders"."status" LIKE '5%'"#
    );
}
