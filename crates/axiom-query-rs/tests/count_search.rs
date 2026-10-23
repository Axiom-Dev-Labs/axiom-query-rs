mod common;

use axiom_query_rs::{ListParams, OrderBy};
use common::setup;
use futures_util::TryStreamExt;
use serde_json::json;

#[tokio::test]
async fn count_matches_list() {
    let (db, engine) = setup().await;
    assert_eq!(engine.count(&db, None).await.unwrap(), 3);
    for (domain, expected) in [
        (json!([["status", "=", "CONFIRMED"]]), 2),
        (
            json!({"or": [["status", "=", "DRAFT"], ["total", ">", 150]]}),
            2,
        ),
        (json!({"not": ["status", "=", "DRAFT"]}), 2),
        (json!([["lines.quantity", ">", 2]]), 1),
        (json!([["customer.name", "=", "Alice"]]), 1),
        (json!([["customer.city.country.name", "=", "India"]]), 1),
    ] {
        let count = engine.count(&db, Some(&domain)).await.unwrap();
        let listed = engine
            .list(&db, &ListParams::new().domain(domain.clone()))
            .await
            .unwrap();
        assert_eq!(count, expected, "{domain}");
        assert_eq!(count as usize, listed.len(), "{domain}");
    }
}

#[tokio::test]
async fn search_streams_matches() {
    let (db, engine) = setup().await;

    let all: Vec<_> = engine
        .search(&db, None, &[])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(all.len(), 3);

    let domain = json!([["customer.name", "=", "Bob"]]);
    let bob: Vec<_> = engine
        .search(&db, Some(&domain), &[])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(bob.iter().map(|o| o.id).collect::<Vec<_>>(), [2]);

    let ordered: Vec<_> = engine
        .search(&db, None, &[OrderBy::desc("total")])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(
        ordered.iter().map(|o| o.total).collect::<Vec<_>>(),
        [200, 100, 50]
    );

    let none = json!([["status", "=", "CANCELLED"]]);
    let empty: Vec<_> = engine
        .search(&db, Some(&none), &[])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert!(empty.is_empty());
}

#[tokio::test]
async fn stopping_a_stream_early_releases_the_connection() {
    let (db, engine) = setup().await;
    {
        let mut stream = Box::pin(engine.search(&db, None, &[]).await.unwrap());
        assert!(stream.try_next().await.unwrap().is_some());
    }
    assert_eq!(engine.count(&db, None).await.unwrap(), 3);
}

#[tokio::test]
async fn search_with_scalar_and_or_domains() {
    let (db, engine) = setup().await;
    let collect = |domain: serde_json::Value| {
        let (db, engine) = (&db, &engine);
        async move {
            let records: Vec<_> = engine
                .search(db, Some(&domain), &[])
                .await
                .unwrap()
                .try_collect()
                .await
                .unwrap();
            records
        }
    };

    let confirmed = collect(json!([["status", "=", "CONFIRMED"]])).await;
    assert_eq!(confirmed.len(), 2);
    assert!(confirmed.iter().all(|o| o.status == "CONFIRMED"));

    let either =
        collect(json!({"or": [["status", "=", "CONFIRMED"], ["status", "=", "DRAFT"]]})).await;
    assert_eq!(either.len(), 3);

    let alice = collect(json!([["customer.name", "=", "Alice"]])).await;
    assert_eq!(alice.iter().map(|o| o.id).collect::<Vec<_>>(), [1]);
}
