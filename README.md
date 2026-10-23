# axiom-query-rs

[![crates.io](https://img.shields.io/crates/v/axiom-query-rs.svg)](https://crates.io/crates/axiom-query-rs)
[![docs.rs](https://img.shields.io/docsrs/axiom-query-rs)](https://docs.rs/axiom-query-rs)
[![license](https://img.shields.io/crates/l/axiom-query-rs.svg)](#license)

Specification-based query and aggregation engine for [SeaORM](https://www.sea-ql.org/SeaORM/) entities.

Define filters as composable data — JSON lists and dicts, or `QuerySpec` trees — and run them against any SeaORM entity without writing SQL. Dot-notation paths filter and group across relations, `read_group` aggregates with `GROUP BY` / `HAVING`, and every group comes back with a domain that selects exactly its records.

A Rust port of the Python [AxiomQuery](#acknowledgements) library.

```rust
let orders = engine
    .list(&db, &ListParams::new().domain(json!([
        ["status", "=", "CONFIRMED"],
        ["customer.city.country.name", "=", "India"],
        ["lines.quantity", ">", 2],
    ])))
    .await?;
```

---

## Install

```bash
cargo add axiom-query-rs
cargo add sea-orm --features sqlx-postgres,runtime-tokio   # pick your driver and runtime
```

`axiom-query-rs` does not enable a database driver; enable one on your own `sea-orm` dependency (`sqlx-postgres`, `sqlx-sqlite` or `sqlx-mysql`, plus a runtime).

Requires Rust 1.94+ and SeaORM 2.0.

---

## Quick start

```rust
use std::sync::Arc;

use axiom_query_rs::{ListParams, OrderBy, QueryEngine, ReadGroupParams, Registry};
use serde_json::json;

// Register every entity that a dot path may reach; the engine's own entity is implicit.
let registry = Arc::new(
    Registry::new()
        .register::<customer::Entity>()
        .register::<order_line::Entity>(),
);
let engine = QueryEngine::<order::Entity>::new(registry);

// list — filter, order, paginate; returns Vec<order::Model>
let orders = engine
    .list(
        &db,
        &ListParams::new()
            .domain(json!([["status", "=", "CONFIRMED"], ["customer.name", "istarts_with", "al"]]))
            .order_by(OrderBy::desc("total"))
            .limit(20),
    )
    .await?;

// count — SELECT COUNT(*), no rows transferred
let n = engine.count(&db, Some(&json!([["lines.quantity", ">", 2]]))).await?;

// search — stream every match
let mut stream = engine.search(&db, None, &[OrderBy::asc("id")]).await?;
while let Some(order) = stream.try_next().await? { /* ... */ }
drop(stream); // the stream holds a pooled connection until dropped

// read_group — GROUP BY + aggregates, with a drill-down domain per group
let groups = engine
    .read_group(&db, &ReadGroupParams::new(["created_at:month"], ["__count", "total:sum"]))
    .await?;
for group in &groups {
    let records = engine
        .list(&db, &ListParams::new().domain(group["__domain"].clone()))
        .await?;
}
```

The full runnable version, including the entity definitions, is [`examples/quickstart.rs`](https://github.com/Axiom-Dev-Labs/axiom-query-rs/blob/main/crates/axiom-query-rs/examples/quickstart.rs):

```bash
cargo run --example quickstart
```

---

## Domain syntax

A **domain** is a JSON expression compiled to a `WHERE` clause. `null` or `[]` matches every record.

### Condition

```json
[field_path, operator, value]
```

| Operator | Meaning | Value |
|----------|---------|-------|
| `=` `!=` | Equality (`null` compiles to `IS NULL` / `IS NOT NULL`) | scalar or `null` |
| `>` `<` `>=` `<=` | Comparison | scalar |
| `in` `not in` | Membership | list |
| `like` `ilike` | Pattern match; `%` and `_` are wildcards | pattern string |
| `contains` `icontains` | Substring | string, matched literally |
| `starts_with` `istarts_with` | Prefix | string, matched literally |
| `ends_with` `iends_with` | Suffix | string, matched literally |
| `is_null` | Null check | `true` / `false` |

Operators are case-insensitive. The `i` variants ignore case.

`contains`, `starts_with` and `ends_with` escape `%`, `_` and `\` in the value, so a search for `50%` matches the text `50%` — use them for user-entered search terms. `like` and `ilike` take a raw pattern.

### Logical composition

```jsonc
// AND — a list of items (implicit)
[["status", "=", "CONFIRMED"], ["total", ">", 100]]

// AND / OR — explicit, at least two items
{"and": [["status", "=", "CONFIRMED"], ["total", ">", 100]]}
{"or":  [["status", "=", "DRAFT"], ["status", "=", "CANCELLED"]]}

// NOT
{"not": ["status", "=", "CANCELLED"]}

// Mixed
[
  {"or": [["status", "=", "CONFIRMED"], ["status", "=", "DRAFT"]]},
  {"not": ["total", "=", 0]}
]
```

The same trees can be built in Rust and composed with `&`, `|` and `!`:

```rust
use axiom_query_rs::{Op, QuerySpec};

let spec = QuerySpec::condition("status", Op::Eq, "CONFIRMED")
    & !QuerySpec::condition("total", Op::Eq, 0);
```

### Relational paths

Dot notation walks relations to any depth. Each hop compiles to a correlated `EXISTS` subquery, so filters on one-to-many relations never duplicate parent rows.

```jsonc
[["lines.quantity", ">", 2]]                       // has_many: any line matches
[["customer.name", "=", "Alice"]]                  // belongs_to: the referenced row matches
[["customer.city.country.name", "=", "India"]]     // N levels deep
[["lines.product.category", "=", "Electronics"]]   // has_many, then belongs_to
```

A path segment is the entity's `Relation` enum variant in snake_case: `Relation::Lines` is `lines`, `Relation::OrderLine` is `order_line`. Each hop uses the relation's full SeaORM definition, including any extra `on_condition`, not just the foreign key.

Every entity a path reaches must be registered in the `Registry`. Reaching an unregistered one fails with `UNREGISTERED_ENTITY`.

### Values

Values are converted to the column's type before the query runs:

- date and datetime strings (`"2026-01-15"`, `"2026-01-15T10:30:00"`, RFC 3339) become `chrono` values
- decimal and UUID strings are parsed
- numeric strings are accepted for integer and float columns

A value that cannot represent the column's type — `"yesterday"` on a datetime, `5` on a text column — fails with `INVALID_FILTER_VALUE` before reaching the database.

---

## `list`, `search`, `count`

```rust
engine.list(&db, &ListParams).await?           // Vec<E::Model>
engine.search(&db, domain, &order_by).await?   // impl Stream<Item = Result<E::Model>>
engine.count(&db, domain).await?               // u64
```

`ListParams` holds `domain`, `order_by`, `limit` and `offset`. `order_by` accepts the entity's own columns. `search` takes no `limit` / `offset`: it is for processing every match.

---

## `read_group`

```rust
let params = ReadGroupParams::new(["status", "created_at:month"], ["__count", "total:sum"])
    .domain(json!([["total", ">", 0]]))      // WHERE
    .having(json!([["__count", ">", 1]]))    // HAVING, on aliases
    .order_by(OrderBy::desc("total__sum"))   // ORDER BY, on aliases
    .limit(10)
    .offset(0);
let groups: Vec<Group> = engine.read_group(&db, &params).await?;
```

| Spec | Form | Result key |
|------|------|------------|
| group by | `field` or `field:granularity` | `field`, `field__granularity` |
| aggregate | `__count` or `field:function` | `__count`, `field__function` |

Dots in paths become `__` in keys: `lines.quantity:sum` → `lines__quantity__sum`.

- **Functions:** `count` `sum` `avg` `min` `max`. `sum` and `avg` require a numeric column.
- **Granularities:** `day` `week` `month` `quarter` `year`, on date and datetime columns. Every backend returns the first day of the period as `YYYY-MM-DD`; weeks start on Monday.
- **Relations:** dot paths in group by and aggregate specs are `LEFT JOIN`ed, so groups without related rows are kept, with `null` values.
- **Fan-out guard:** at most one one-to-many relation may be joined, because a second one would multiply rows and double-count. Otherwise the query fails with `MULTI_CHILD_AGGREGATION`.

### `__domain` drill-down

Every group carries a `__domain` that selects exactly its records — the group's values, plus your original domain:

```jsonc
{
  "status": "CONFIRMED",
  "created_at__month": "2026-01-01",
  "__count": 2,
  "__domain": [
    ["status", "=", "CONFIRMED"],
    ["created_at", ">=", "2026-01-01"],
    ["created_at", "<", "2026-02-01"],
    ["total", ">", 0]
  ]
}
```

Pass it back to `list`, `search` or `count`.

---

## JSON request bodies

`ListParams` and `ReadGroupParams` implement `serde::Deserialize`, so an HTTP handler can pass a request body straight through:

```json
{
  "groupby": ["status"],
  "aggregates": ["__count", "lines.quantity:sum"],
  "domain": [["customer.city.name", "=", "Mumbai"]],
  "having": [["__count", ">", 1]],
  "order_by": [["__count", "desc"]],
  "limit": 10
}
```

`aggregates` defaults to `["__count"]`. Unknown keys are rejected. `order_by` items are `"field"` or `["field", "asc" | "desc"]`.

---

## Using the compiled SQL directly

The engine also exposes what it would run, for composing with your own SeaORM queries:

```rust
let condition = engine.condition(&domain, DbBackend::Postgres)?;               // sea_query::Condition
let select = engine.select(&params, DbBackend::Postgres)?;                     // sea_orm::Select<E>
let stmt = engine.read_group_statement(&params, DbBackend::Postgres)?;         // SelectStatement
let stmt = engine.count_statement(Some(&domain), DbBackend::Postgres)?;        // SelectStatement
```

`parse_domain`, `parse_read_group` and `build_group_domain` are public as well.

---

## Errors

Invalid specifications fail with a `QueryError` before any SQL reaches the database. `err.code()` returns a stable `ErrorCode`, and `err.code().as_str()` gives its string form:

| Code | Raised when |
|------|-------------|
| `INVALID_DOMAIN` | Malformed domain or unknown operator |
| `INVALID_FILTER_FIELD` | Unknown field or relation in a path |
| `INVALID_FILTER_VALUE` | Value incompatible with the operator or column type |
| `INVALID_AGGREGATION` | Bad aggregate or group-by spec, or wrong column type for it |
| `INVALID_READ_GROUP` | Malformed `read_group` request |
| `INVALID_HAVING_FIELD` / `INVALID_HAVING_OPERATOR` | HAVING on an unknown alias, or with an unsupported operator |
| `INVALID_ORDER_BY` | ORDER BY on an unknown field or alias |
| `MULTI_CHILD_AGGREGATION` | `read_group` joins more than one one-to-many relation |
| `UNREGISTERED_ENTITY` | A path reaches an entity missing from the `Registry` |
| `DATABASE_ERROR` | The database returned an error (`QueryError::Db`) |

```rust
match engine.list(&db, &params).await {
    Err(err) => eprintln!("{}: {}", err.code(), err.message()),
    Ok(records) => { /* ... */ }
}
```

---

## Database support

| Backend | Status |
|---------|--------|
| PostgreSQL | Tested (full test suite) |
| SQLite | Tested (full test suite) |
| MySQL / MariaDB | SQL generation tested; not yet run against a server |

- **SQLite case sensitivity:** `LIKE` ignores case for ASCII letters, so `like`, `contains`, `starts_with` and `ends_with` don't distinguish case there.
- **MySQL case sensitivity:** depends on the column collation.
- **PostgreSQL:** these operators are case-sensitive.

---

## Development

```bash
cargo test                     # in-memory SQLite
cargo clippy --all-targets
cargo run --example quickstart
```

Run the same suite against PostgreSQL by pointing `AXIOM_TEST_DATABASE_URL` at a database you can create schemas in:

```bash
createdb axiom_query_test
AXIOM_TEST_DATABASE_URL='postgres://USER@%2Fvar%2Frun%2Fpostgresql/axiom_query_test' cargo test
```

Each test creates its own schema, `aq_<test file>_<n>`, recreated on every run.

CI (`.github/workflows/ci.yml`) runs formatting, clippy, docs, the test suite on SQLite and PostgreSQL 16, an MSRV check and a package dry run on every push to `main` and every pull request.

### Releasing

Publishing a GitHub release publishes the crate to crates.io (`.github/workflows/release.yml`). The workflow:

1. checks that the release tag matches the crate version,
2. re-runs CI,
3. runs `cargo publish`.

```bash
# bump `version` in Cargo.toml ([workspace.package]), then:
cargo check                      # refresh Cargo.lock
git commit -am "Release 0.2.0"
git push origin main
gh release create v0.2.0 --generate-notes   # or GitHub → Releases → Draft a new release
```

One-time setup: create a crates.io API token with the `publish-new` and `publish-update` scopes, and add it as the `CARGO_REGISTRY_TOKEN` secret of the repository's `crates-io` environment (Settings → Environments).

---

## Acknowledgements

- **AxiomQuery** (Python, for SQLAlchemy 2.0) — the library this crate ports: the domain language, `read_group` and `__domain` drill-down.
- **[Odoo ORM domains](https://www.odoo.com/documentation/latest/developer/reference/backend/orm.html#search-domains)** — the `[field, operator, value]` triple and the shape of `read_group`.
- **[The Specification pattern](https://martinfowler.com/apsupp/spec.pdf)** by Martin Fowler and Eric Evans — business rules as composable objects.
- **[SeaORM](https://www.sea-ql.org/SeaORM/) and [SeaQuery](https://github.com/SeaQL/sea-query)** — entity metadata and the dynamic SQL builder this engine compiles to.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this crate by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
