mod common;

use axiom_query_rs::{ErrorCode, ListParams, OrderBy};
use common::{ids, setup};
use serde_json::json;

async fn list_ids(domain: serde_json::Value) -> Vec<i32> {
    let (db, engine) = setup().await;
    ids(&engine
        .list(&db, &ListParams::new().domain(domain))
        .await
        .unwrap())
}

#[tokio::test]
async fn returns_all_records() {
    let (db, engine) = setup().await;
    assert_eq!(engine.list(&db, &ListParams::new()).await.unwrap().len(), 3);
}

#[tokio::test]
async fn simple_and_logical_filters() {
    assert_eq!(
        list_ids(json!([["status", "=", "CONFIRMED"]])).await,
        [1, 2]
    );
    assert_eq!(
        list_ids(json!({"or": [["status", "=", "CONFIRMED"], ["status", "=", "DRAFT"]]})).await,
        [1, 2, 3]
    );
    assert_eq!(
        list_ids(json!({"not": ["status", "=", "DRAFT"]})).await,
        [1, 2]
    );
    assert_eq!(
        list_ids(json!([["status", "in", ["DRAFT", "CANCELLED"]]])).await,
        [3]
    );
    assert_eq!(
        list_ids(json!([["status", "not in", ["DRAFT"]]])).await,
        [1, 2]
    );
    assert_eq!(
        list_ids(json!([["customer_id", "is_null", true]])).await,
        [3]
    );
    assert_eq!(list_ids(json!([["customer_id", "=", null]])).await, [3]);
    assert_eq!(list_ids(json!([["status", "like", "CONF%"]])).await, [1, 2]);
    assert_eq!(list_ids(json!([])).await, [1, 2, 3]);
}

#[tokio::test]
async fn date_strings_are_coerced() {
    assert_eq!(
        list_ids(json!([["created_at", ">=", "2026-02-01"]])).await,
        [2]
    );
    assert_eq!(
        list_ids(json!([
            ["created_at", ">=", "2026-01-01"],
            ["created_at", "<", "2026-02-01T00:00:00"]
        ]))
        .await,
        [1, 3]
    );
}

#[tokio::test]
async fn child_field_exists() {
    assert_eq!(list_ids(json!([["lines.quantity", ">", 2]])).await, [1]);
    assert_eq!(
        list_ids(json!({"not": ["lines.quantity", ">", 0]})).await,
        [3]
    );
}

#[tokio::test]
async fn limit_offset_order_by() {
    let (db, engine) = setup().await;
    let page = engine.list(&db, &ListParams::new().limit(2)).await.unwrap();
    assert_eq!(page.len(), 2);

    let ordered = engine
        .list(&db, &ListParams::new().order_by(OrderBy::desc("total")))
        .await
        .unwrap();
    assert_eq!(
        ordered.iter().map(|o| o.total).collect::<Vec<_>>(),
        [200, 100, 50]
    );

    let second = engine
        .list(
            &db,
            &ListParams::new()
                .order_by(OrderBy::asc("total"))
                .limit(1)
                .offset(1),
        )
        .await
        .unwrap();
    assert_eq!(second[0].total, 100);
}

#[tokio::test]
async fn many_to_one_paths() {
    assert_eq!(
        list_ids(json!([["customer.name", "=", "Alice"]])).await,
        [1]
    );
    assert_eq!(
        list_ids(json!([["customer.name", "ilike", "%OB%"]])).await,
        [2]
    );
    assert_eq!(
        list_ids(json!([["customer.name", "=", "Nobody"]])).await,
        Vec::<i32>::new()
    );
    assert_eq!(
        list_ids(json!([
            ["customer.name", "=", "Alice"],
            ["status", "=", "CONFIRMED"]
        ]))
        .await,
        [1]
    );
}

#[tokio::test]
async fn n_level_paths() {
    assert_eq!(
        list_ids(json!([["customer.city.name", "=", "Mumbai"]])).await,
        [1]
    );
    assert_eq!(
        list_ids(json!([["customer.city.country.name", "ilike", "%Ind%"]])).await,
        [1]
    );
    assert_eq!(
        list_ids(json!([["lines.product.category", "=", "Electronics"]])).await,
        [2]
    );
    assert_eq!(
        list_ids(json!([["customer.city.country.name", "=", "Atlantis"]])).await,
        Vec::<i32>::new()
    );
}

#[tokio::test]
async fn invalid_specs_fail_before_the_database() {
    let (db, engine) = setup().await;
    for (domain, code) in [
        (
            json!([["unknown_field", "=", "x"]]),
            ErrorCode::InvalidFilterField,
        ),
        (
            json!([["nonexistent.name", "=", "x"]]),
            ErrorCode::InvalidFilterField,
        ),
        (
            json!([["customer.nope.name", "=", "x"]]),
            ErrorCode::InvalidFilterField,
        ),
        (
            json!([["customer.city.bogus", "=", "x"]]),
            ErrorCode::InvalidFilterField,
        ),
        (
            json!([["created_at", ">", "yesterday"]]),
            ErrorCode::InvalidFilterValue,
        ),
        (
            json!([["status", "in", "DRAFT"]]),
            ErrorCode::InvalidFilterValue,
        ),
        (json!([["total", ">", null]]), ErrorCode::InvalidFilterValue),
        (json!([["status", "~", "x"]]), ErrorCode::InvalidDomain),
    ] {
        let err = engine
            .list(&db, &ListParams::new().domain(domain.clone()))
            .await
            .unwrap_err();
        assert_eq!(err.code(), code, "{domain}: {err}");
    }
    let err = engine
        .list(&db, &ListParams::new().order_by(OrderBy::asc("nope")))
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidOrderBy);
}

#[tokio::test]
async fn params_deserialize_from_json() {
    let (db, engine) = setup().await;
    let params: ListParams = serde_json::from_value(json!({
        "domain": [["status", "=", "CONFIRMED"]],
        "order_by": [["total", "desc"]],
        "limit": 1,
    }))
    .unwrap();
    let records = engine.list(&db, &params).await.unwrap();
    assert_eq!(records.iter().map(|o| o.id).collect::<Vec<_>>(), [2]);
}

#[tokio::test]
async fn nested_path_combined_with_scalar() {
    assert_eq!(
        list_ids(json!([
            ["customer.city.name", "=", "Mumbai"],
            ["status", "=", "CONFIRMED"]
        ]))
        .await,
        [1]
    );
}

#[tokio::test]
async fn text_match_operators() {
    assert_eq!(
        list_ids(json!([["status", "contains", "FIRM"]])).await,
        [1, 2]
    );
    assert_eq!(
        list_ids(json!([["status", "starts_with", "DR"]])).await,
        [3]
    );
    assert_eq!(
        list_ids(json!([["status", "ends_with", "MED"]])).await,
        [1, 2]
    );
    assert_eq!(
        list_ids(json!([["status", "ends_with", "CONF"]])).await,
        Vec::<i32>::new()
    );
    assert_eq!(
        list_ids(json!([["customer.name", "icontains", "LIC"]])).await,
        [1]
    );
    assert_eq!(
        list_ids(json!([["customer.name", "istarts_with", "bO"]])).await,
        [2]
    );
    assert_eq!(
        list_ids(json!([["customer.city.name", "iends_with", "YORK"]])).await,
        [2]
    );
    assert_eq!(
        list_ids(json!({"not": ["status", "contains", "FIRM"]})).await,
        [3]
    );
}

#[tokio::test]
async fn text_match_values_are_literal() {
    use common::customer;
    use sea_orm::ConnectionTrait;

    let db = common::seeded_db().await;
    db.execute_unprepared(r"INSERT INTO customers (id, name) VALUES (3, '50%_off\deal')")
        .await
        .unwrap();
    let engine = axiom_query_rs::QueryEngine::<customer::Entity>::new(common::registry());
    let names = |domain: serde_json::Value| {
        let (db, engine) = (&db, &engine);
        async move {
            let mut names: Vec<_> = engine
                .list(db, &ListParams::new().domain(domain))
                .await
                .unwrap()
                .into_iter()
                .map(|c| c.name)
                .collect();
            names.sort();
            names
        }
    };

    // `like` treats `%` and `_` as wildcards; the literal operators do not.
    assert_eq!(names(json!([["name", "like", "%"]])).await.len(), 3);
    assert_eq!(
        names(json!([["name", "contains", "%"]])).await,
        [r"50%_off\deal"]
    );
    assert_eq!(
        names(json!([["name", "contains", "0%_o"]])).await,
        [r"50%_off\deal"]
    );
    assert_eq!(
        names(json!([["name", "starts_with", "_"]])).await,
        Vec::<String>::new()
    );
    assert_eq!(
        names(json!([["name", "ends_with", r"\deal"]])).await,
        [r"50%_off\deal"]
    );
    assert_eq!(
        names(json!([["name", "icontains", "%_OFF"]])).await,
        [r"50%_off\deal"]
    );
}

#[tokio::test]
async fn text_match_requires_a_string() {
    let (db, engine) = setup().await;
    let err = engine
        .list(
            &db,
            &ListParams::new().domain(json!([["status", "contains", 5]])),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidFilterValue);
}
