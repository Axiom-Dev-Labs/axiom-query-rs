mod common;

use std::collections::BTreeMap;

use axiom_query_rs::{ErrorCode, Group, ListParams, OrderBy, ReadGroupParams};
use common::setup;
use serde_json::{Value, json};

fn by<'g>(groups: &'g [Group], key: &str) -> BTreeMap<String, &'g Group> {
    groups
        .iter()
        .map(|g| {
            (
                g[key]
                    .as_str()
                    .map_or_else(|| g[key].to_string(), str::to_owned),
                g,
            )
        })
        .collect()
}

#[tokio::test]
async fn count_by_status() {
    let (db, engine) = setup().await;
    let groups = engine
        .read_group(&db, &ReadGroupParams::new(["status"], ["__count"]))
        .await
        .unwrap();
    assert_eq!(groups.len(), 2);
    let g = by(&groups, "status");
    assert_eq!(g["CONFIRMED"]["__count"], 2);
    assert_eq!(g["DRAFT"]["__count"], 1);
}

#[tokio::test]
async fn with_domain() {
    let (db, engine) = setup().await;
    let params =
        ReadGroupParams::new(["status"], ["__count"]).domain(json!([["status", "=", "CONFIRMED"]]));
    let groups = engine.read_group(&db, &params).await.unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["__count"], 2);
}

#[tokio::test]
async fn date_granularities() {
    let (db, engine) = setup().await;
    let counts = |groups: Vec<Group>, key: &str| -> BTreeMap<String, i64> {
        groups
            .into_iter()
            .map(|g| {
                (
                    g[key].as_str().unwrap().to_owned(),
                    g["__count"].as_i64().unwrap(),
                )
            })
            .collect()
    };

    let month = engine
        .read_group(
            &db,
            &ReadGroupParams::new(["created_at:month"], ["__count"]),
        )
        .await
        .unwrap();
    assert_eq!(
        counts(month, "created_at__month"),
        BTreeMap::from([("2026-01-01".into(), 2), ("2026-02-01".into(), 1)])
    );

    let quarter = engine
        .read_group(
            &db,
            &ReadGroupParams::new(["created_at:quarter"], ["__count"]),
        )
        .await
        .unwrap();
    assert_eq!(
        counts(quarter, "created_at__quarter"),
        BTreeMap::from([("2026-01-01".into(), 3)])
    );

    let year = engine
        .read_group(&db, &ReadGroupParams::new(["created_at:year"], ["__count"]))
        .await
        .unwrap();
    assert_eq!(
        counts(year, "created_at__year"),
        BTreeMap::from([("2026-01-01".into(), 3)])
    );

    // 2026-01-15 is a Thursday, 2026-01-25 a Sunday, 2026-02-20 a Friday: weeks start Monday.
    let week = engine
        .read_group(&db, &ReadGroupParams::new(["created_at:week"], ["__count"]))
        .await
        .unwrap();
    assert_eq!(
        counts(week, "created_at__week"),
        BTreeMap::from([
            ("2026-01-12".into(), 1),
            ("2026-01-19".into(), 1),
            ("2026-02-16".into(), 1)
        ])
    );

    let day = engine
        .read_group(&db, &ReadGroupParams::new(["created_at:day"], ["__count"]))
        .await
        .unwrap();
    assert_eq!(counts(day, "created_at__day").len(), 3);
}

#[tokio::test]
async fn month_drilldown_selects_the_group() {
    let (db, engine) = setup().await;
    let groups = engine
        .read_group(
            &db,
            &ReadGroupParams::new(["created_at:month"], ["__count"]),
        )
        .await
        .unwrap();
    for group in &groups {
        let records = engine
            .list(&db, &ListParams::new().domain(group["__domain"].clone()))
            .await
            .unwrap();
        assert_eq!(Value::from(records.len()), group["__count"], "{group:?}");
    }
}

#[tokio::test]
async fn child_sum_left_join() {
    let (db, engine) = setup().await;
    let groups = engine
        .read_group(
            &db,
            &ReadGroupParams::new(["status"], ["lines.quantity:sum"]),
        )
        .await
        .unwrap();
    let g = by(&groups, "status");
    assert_eq!(g["CONFIRMED"]["lines__quantity__sum"], 6);
    assert_eq!(g["DRAFT"]["lines__quantity__sum"], Value::Null);
}

#[tokio::test]
async fn having_and_order_by() {
    let (db, engine) = setup().await;
    let params = ReadGroupParams::new(["status"], ["__count"]).having(json!([["__count", ">", 1]]));
    let groups = engine.read_group(&db, &params).await.unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["status"], "CONFIRMED");

    let params = ReadGroupParams::new(
        ["status"],
        ["__count", "total:sum", "total:max", "total:avg"],
    )
    .order_by(OrderBy::asc("total__sum"))
    .limit(1);
    let groups = engine.read_group(&db, &params).await.unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["status"], "DRAFT");
    assert_eq!(groups[0]["total__max"], 50);
}

#[tokio::test]
async fn domain_drilldown() {
    let (db, engine) = setup().await;
    let domain = json!([["total", ">", 0]]);
    let groups = engine
        .read_group(
            &db,
            &ReadGroupParams::new(["status"], ["__count"]).domain(domain),
        )
        .await
        .unwrap();
    let confirmed = by(&groups, "status")["CONFIRMED"];
    assert_eq!(
        confirmed["__domain"],
        json!([["status", "=", "CONFIRMED"], ["total", ">", 0]])
    );
    let records = engine
        .list(
            &db,
            &ListParams::new().domain(confirmed["__domain"].clone()),
        )
        .await
        .unwrap();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|r| r.status == "CONFIRMED"));
}

#[tokio::test]
async fn n_level_groupby() {
    let (db, engine) = setup().await;
    let groups = engine
        .read_group(
            &db,
            &ReadGroupParams::new(["lines.product.category"], ["lines.quantity:sum"]),
        )
        .await
        .unwrap();
    let g = by(&groups, "lines__product__category");
    assert_eq!(g["Hardware"]["lines__quantity__sum"], 5);
    assert_eq!(g["Electronics"]["lines__quantity__sum"], 1);

    let groups = engine
        .read_group(
            &db,
            &ReadGroupParams::new(["customer.city.name"], ["__count"]),
        )
        .await
        .unwrap();
    let g = by(&groups, "customer__city__name");
    assert_eq!(g["Mumbai"]["__count"], 1);
    assert_eq!(g["New York"]["__count"], 1);
    assert_eq!(g["null"]["__count"], 1);
    assert_eq!(
        g["null"]["__domain"],
        json!([["customer.city.name", "is_null", true]])
    );
}

#[tokio::test]
async fn invalid_read_groups() {
    let (db, engine) = setup().await;
    for (params, code) in [
        (
            ReadGroupParams::new(["status"], ["status:sum"]),
            ErrorCode::InvalidAggregation,
        ),
        (
            ReadGroupParams::new(["status:month"], ["__count"]),
            ErrorCode::InvalidAggregation,
        ),
        (
            ReadGroupParams::new(["nope"], ["__count"]),
            ErrorCode::InvalidFilterField,
        ),
        (
            ReadGroupParams::new(["status"], Vec::<String>::new()),
            ErrorCode::InvalidReadGroup,
        ),
        (
            ReadGroupParams::new(["status"], ["__count"]).having(json!([["nope", ">", 1]])),
            ErrorCode::InvalidHavingField,
        ),
        (
            ReadGroupParams::new(["status"], ["__count"]).having(json!([["status", "like", "C%"]])),
            ErrorCode::InvalidHavingOperator,
        ),
        (
            ReadGroupParams::new(["status"], ["__count"]).order_by(OrderBy::asc("total")),
            ErrorCode::InvalidOrderBy,
        ),
    ] {
        let err = engine.read_group(&db, &params).await.unwrap_err();
        assert_eq!(err.code(), code, "{params:?}: {err}");
    }
}

#[tokio::test]
async fn rejects_fan_out_across_two_one_to_many_relations() {
    use axiom_query_rs::{QueryEngine, Registry};
    use common::{customer, order};
    use std::sync::Arc;

    // A single one-to-many edge is allowed, even behind a many-to-one hop.
    let registry = Arc::new(
        Registry::new()
            .register::<order::Entity>()
            .register::<customer::Entity>(),
    );
    let engine = QueryEngine::<common::order_line::Entity>::new(registry);
    let db = common::seeded_db().await;
    let params = ReadGroupParams::new(["order.status"], ["__count"]);
    let groups = engine.read_group(&db, &params).await.unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["__count"], 3);

    let engine = common::engine();
    let params = ReadGroupParams::new(["lines.order.lines.product_name"], ["__count"]);
    let err = engine.read_group(&db, &params).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::MultiChildAggregation);
}
