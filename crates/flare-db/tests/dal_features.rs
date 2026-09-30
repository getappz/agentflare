//! Field policies, auto fields, projection, typed errors, validation, events,
//! upsert, search and richer filters -- live in-memory SQLite.
//!
//! Several tests destructure generated structs *exhaustively* (no `..`): those are the
//! compile-time assertions that e.g. `OrderPatch` has no readonly/immutable field and
//! `OrderPublic` has no hidden one.

#![cfg(feature = "sqlite")]

use flare_db::{
    Crud, CrudError, FilterOp, MutationEvent, MutationKind, Page, Validate, ValidationError,
};
use time::OffsetDateTime;

#[derive(sqlx::FromRow, Crud)]
#[crud(table = "orders", pk = "id", soft_delete, validate, unique(email))]
#[allow(dead_code)]
struct Order {
    #[crud(id(prefix = "ord"))]
    id: String,
    #[crud(immutable, searchable)]
    email: String,
    #[crud(searchable)]
    title: String,
    #[crud(immutable)]
    currency: String,
    #[crud(enum_values("open", "paid"), default = "open".to_string())]
    status: String,
    #[crud(readonly)]
    version: i64,
    #[crud(hidden)]
    secret: String,
    #[crud(computed)]
    label: String,
    #[crud(created_at)]
    created_at: OffsetDateTime,
    #[crud(updated_at)]
    updated_at: OffsetDateTime,
    deleted_at: Option<OffsetDateTime>,
    amount: i64,
}

impl Validate for OrderNew {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.amount < 0 {
            return Err(ValidationError::field("amount", "must be >= 0"));
        }
        Ok(())
    }
}

impl Validate for OrderPatch {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.amount.is_some_and(|a| a < 0) {
            return Err(ValidationError::field("amount", "must be >= 0"));
        }
        Ok(())
    }
}

// Events get their own table so a process-wide sink shared by parallel tests can
// be filtered by `object`.
#[derive(sqlx::FromRow, Crud)]
#[crud(table = "audited", pk = "id", soft_delete)]
#[allow(dead_code)]
struct Audited {
    id: i64,
    name: String,
    deleted_at: Option<OffsetDateTime>,
}

async fn pool() -> flare_db::Pool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for ddl in [
        "CREATE TABLE orders (
            id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE, title TEXT NOT NULL,
            currency TEXT NOT NULL, status TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1,
            secret TEXT NOT NULL, created_at TIMESTAMP NOT NULL, updated_at TIMESTAMP NOT NULL,
            deleted_at TIMESTAMP NULL, amount INTEGER NOT NULL)",
        "CREATE TABLE audited (
            id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, deleted_at TIMESTAMP NULL)",
        "CREATE TABLE parent (id INTEGER PRIMARY KEY)",
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parent(id))",
    ] {
        sqlx::query(ddl).execute(&pool).await.unwrap();
    }
    pool
}

fn new_order(email: &str, title: &str) -> OrderNew {
    OrderNew {
        id: None,
        email: email.into(),
        title: title.into(),
        currency: "USD".into(),
        status: None,
        secret: "s3cret".into(),
        amount: 10,
    }
}

trait MustFail {
    fn must_fail(self) -> sqlx::Error;
}

impl<T> MustFail for sqlx::Result<T> {
    fn must_fail(self) -> sqlx::Error {
        match self {
            Err(e) => e,
            Ok(_) => panic!("expected an error"),
        }
    }
}

async fn nap() {
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

#[tokio::test]
async fn policies_shape_the_generated_types() {
    let pool = pool().await;
    let row = Order::create_one(&pool, new_order("a@x.io", "first"))
        .await
        .unwrap();

    // Exhaustive destructure: compiles only if `secret` (hidden) is absent and the
    // computed `label` is present.
    let OrderPublic {
        id,
        email,
        title: _,
        currency: _,
        status,
        version,
        label,
        created_at,
        updated_at,
        deleted_at,
        amount: _,
    } = row;
    assert!(id.starts_with("ord_"), "generated prefixed id, got {id}");
    assert_eq!(email, "a@x.io");
    assert_eq!(status, "open", "default applied when unset");
    assert_eq!(version, 1, "readonly column keeps its DB default");
    assert_eq!(label, "", "computed is Default, never read from SQL");
    assert!(deleted_at.is_none());
    assert!(updated_at >= created_at);

    // Exhaustive: no email/currency (immutable), version (readonly), timestamps, id.
    let OrderPatch {
        title: _,
        status: _,
        secret: _,
        amount: _,
    } = OrderPatch::default();

    // The hidden column is stored but no returned type exposes it, and it is never SELECTed.
    assert!(!Order::COLUMNS.contains(&"secret"));
    assert!(!Order::COLUMNS.contains(&"label"));
    let stored: String = sqlx::query_scalar("SELECT secret FROM orders WHERE id = ?")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, "s3cret");
}

#[tokio::test]
async fn caller_supplied_id_and_status_are_respected() {
    let pool = pool().await;
    let mut n = new_order("b@x.io", "t");
    n.id = Some("ord_custom".into());
    n.status = Some("paid".into());
    let row = Order::create_one(&pool, n).await.unwrap();
    assert_eq!(row.id, "ord_custom");
    assert_eq!(row.status, "paid");
}

#[tokio::test]
async fn updated_at_moves_on_update_one_and_update_where_but_created_at_does_not() {
    let pool = pool().await;
    let a = Order::create_one(&pool, new_order("c@x.io", "t"))
        .await
        .unwrap();
    nap().await;
    let b = Order::update_one(
        &pool,
        a.id.clone(),
        OrderPatch {
            title: Some("t2".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(b.created_at, a.created_at);
    assert!(b.updated_at > a.updated_at);

    nap().await;
    let n = Order::update_where(
        &pool,
        OrderFilter {
            id: Some(FilterOp::Eq(a.id.clone())),
            ..Default::default()
        },
        OrderPatch {
            amount: Some(99),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(n, 1);
    let c = Order::get(&pool, a.id.clone()).await.unwrap().unwrap();
    assert!(
        c.updated_at > b.updated_at,
        "update_where must bump updated_at"
    );
    assert_eq!(c.amount, 99);

    // An empty patch is a no-op and must not bump the timestamp.
    nap().await;
    let d = Order::update_one(&pool, a.id.clone(), OrderPatch::default())
        .await
        .unwrap();
    assert_eq!(d.updated_at, c.updated_at);
}

#[tokio::test]
async fn select_projection_returns_only_requested_columns() {
    let pool = pool().await;
    let a = Order::create_one(&pool, new_order("d@x.io", "proj"))
        .await
        .unwrap();
    let p = Order::get_select(&pool, a.id.clone(), &[OrderField::Id, OrderField::Title])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.id.as_deref(), Some(a.id.as_str()));
    assert_eq!(p.title.as_deref(), Some("proj"));
    assert!(p.amount.is_none() && p.email.is_none());

    let rows = Order::list_select(
        &pool,
        OrderFilter::default(),
        Page::default(),
        &[OrderField::Amount],
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].amount, Some(10));
    assert!(rows[0].title.is_none());

    // Empty projection = every visible column.
    let all = Order::get_select(&pool, a.id.clone(), &[])
        .await
        .unwrap()
        .unwrap();
    assert!(all.title.is_some() && all.deleted_at == Some(None));
}

#[tokio::test]
async fn errors_are_typed() {
    let pool = pool().await;
    Order::create_one(&pool, new_order("dup@x.io", "t"))
        .await
        .unwrap();

    let err = Order::create_one(&pool, new_order("dup@x.io", "t2"))
        .await
        .must_fail();
    match CrudError::from(err) {
        CrudError::Conflict { table, cols, .. } => {
            assert_eq!(table.as_deref(), Some("orders"));
            assert_eq!(cols, ["email"]);
        }
        other => panic!("expected Conflict, got {other:?}"),
    }

    let err = Order::update_one(
        &pool,
        "ord_missing".into(),
        OrderPatch {
            amount: Some(1),
            ..Default::default()
        },
    )
    .await
    .must_fail();
    assert!(matches!(CrudError::from(err), CrudError::NotFound));

    let err = sqlx::query("INSERT INTO audited (name) VALUES (NULL)")
        .execute(&pool)
        .await
        .must_fail();
    match CrudError::from(err) {
        CrudError::NotNull { col, .. } => assert_eq!(col.as_deref(), Some("name")),
        other => panic!("expected NotNull, got {other:?}"),
    }

    let err = sqlx::query("INSERT INTO child (parent_id) VALUES (42)")
        .execute(&pool)
        .await
        .must_fail();
    assert!(matches!(CrudError::from(err), CrudError::ForeignKey { .. }));
}

#[tokio::test]
async fn validation_runs_before_any_write() {
    let pool = pool().await;
    let mut bad = new_order("v@x.io", "t");
    bad.status = Some("bogus".into());
    let err = CrudError::from(Order::create_one(&pool, bad).await.must_fail());
    let CrudError::Validation(v) = err else {
        panic!("expected Validation");
    };
    assert_eq!(v.field, Some("status"));
    assert!(v.message.contains("open, paid"));

    let mut neg = new_order("v@x.io", "t");
    neg.amount = -1;
    assert!(matches!(
        CrudError::from(Order::create_one(&pool, neg).await.must_fail()),
        CrudError::Validation(_)
    ));
    assert_eq!(
        Order::count(&pool).await.unwrap(),
        0,
        "nothing was inserted"
    );

    let ok = Order::create_one(&pool, new_order("v@x.io", "t"))
        .await
        .unwrap();
    let err = Order::update_one(
        &pool,
        ok.id.clone(),
        OrderPatch {
            status: Some("nope".into()),
            ..Default::default()
        },
    )
    .await
    .must_fail();
    assert!(matches!(CrudError::from(err), CrudError::Validation(_)));
    let err = Order::update_where(
        &pool,
        OrderFilter::default(),
        OrderPatch {
            amount: Some(-5),
            ..Default::default()
        },
    )
    .await
    .must_fail();
    assert!(matches!(CrudError::from(err), CrudError::Validation(_)));
    assert_eq!(Order::get(&pool, ok.id).await.unwrap().unwrap().amount, 10);
}

#[tokio::test]
async fn upsert_never_overwrites_immutable_readonly_or_unset_defaults() {
    let pool = pool().await;
    let mut first = new_order("u@x.io", "original");
    first.status = Some("paid".into());
    let a = Order::upsert_one(&pool, first).await.unwrap();

    nap().await;
    let mut second = new_order("u@x.io", "changed");
    second.currency = "EUR".into(); // immutable: must be ignored on conflict
    second.amount = 77;
    second.secret = "rotated".into();
    let b = Order::upsert_one(&pool, second).await.unwrap();

    assert_eq!(b.id, a.id, "conflict updates the existing row");
    assert_eq!(Order::count(&pool).await.unwrap(), 1);
    assert_eq!(b.title, "changed");
    assert_eq!(b.amount, 77);
    assert_eq!(b.currency, "USD", "immutable column must survive an upsert");
    assert_eq!(
        b.status, "paid",
        "unset default must not clobber the stored value"
    );
    assert_eq!(b.created_at, a.created_at);
    assert!(b.updated_at > a.updated_at);
    let secret: String = sqlx::query_scalar("SELECT secret FROM orders WHERE id = ?")
        .bind(&a.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(secret, "rotated", "mutable hidden columns are upserted");

    // Explicit target + batch.
    let rows = Order::upsert_many_on(
        &pool,
        &[OrderField::Email],
        vec![new_order("u@x.io", "t3"), new_order("w@x.io", "new")],
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(Order::count(&pool).await.unwrap(), 2);

    // Nothing to update but the key still returns the row.
    let err = Order::upsert_many_on(&pool, &[], vec![new_order("z@x.io", "t")])
        .await
        .must_fail();
    assert!(matches!(CrudError::from(err), CrudError::Validation(_)));
}

#[tokio::test]
async fn q_search_is_case_insensitive_or_across_fields_and_escapes_wildcards() {
    let pool = pool().await;
    for (email, title) in [
        ("ann@x.io", "Widget 100%"),
        ("bob@x.io", "Gadget_v2"),
        ("cat@x.io", "Other thing"),
        ("dan@x.io", "Plain"),
    ] {
        Order::create_one(&pool, new_order(email, title))
            .await
            .unwrap();
    }
    let search = |q: &str| OrderFilter {
        q: Some(q.into()),
        ..Default::default()
    };
    let count = |q: &'static str| {
        let pool = pool.clone();
        async move { Order::count_where(&pool, search(q)).await.unwrap() }
    };
    assert_eq!(count("WIDGET").await, 1, "case-insensitive");
    assert_eq!(count("cat@").await, 1, "matches the email field too");
    assert_eq!(count("%").await, 1, "% is literal, not a wildcard");
    assert_eq!(count("_").await, 1, "_ is literal, not a wildcard");
    assert_eq!(count("\\").await, 0, "backslash is literal");
    assert_eq!(count("   ").await, 4, "blank q restricts nothing");
    assert_eq!(count("x.io").await, 4);
    assert_eq!(count("100%' OR '1'='1").await, 0, "values are bound");
}

#[tokio::test]
async fn richer_filter_operators_and_boolean_tree() {
    let pool = pool().await;
    for (i, amount) in [10_i64, 20, 30, 40].into_iter().enumerate() {
        let mut n = new_order(&format!("f{i}@x.io"), &format!("Item {i}"));
        n.amount = amount;
        Order::create_one(&pool, n).await.unwrap();
    }
    let by = |op: FilterOp<i64>| OrderFilter {
        amount: Some(op),
        ..Default::default()
    };
    let n = |f: OrderFilter| {
        let pool = pool.clone();
        async move { Order::count_where(&pool, f).await.unwrap() }
    };
    assert_eq!(n(by(FilterOp::Gt(20))).await, 2);
    assert_eq!(n(by(FilterOp::Gte(20))).await, 3);
    assert_eq!(n(by(FilterOp::Lt(20))).await, 1);
    assert_eq!(n(by(FilterOp::Lte(20))).await, 2);
    assert_eq!(n(by(FilterOp::Between(20, 30))).await, 2);

    let ilike = OrderFilter {
        title: Some(FilterOp::ILike("item _".into())),
        ..Default::default()
    };
    assert_eq!(n(ilike).await, 4);
    let ilike_escaped = OrderFilter {
        title: Some(FilterOp::ILike("item \\_".into())),
        ..Default::default()
    };
    assert_eq!(n(ilike_escaped).await, 0, "escaped underscore is literal");

    // (amount < 20 OR amount > 30) AND title ILIKE 'item%'
    let tree = OrderFilter {
        title: Some(FilterOp::ILike("ITEM%".into())),
        or: vec![by(FilterOp::Lt(20)), by(FilterOp::Gt(30))],
        ..Default::default()
    };
    assert_eq!(n(tree).await, 2);
    // and: [amount >= 20, amount <= 30]
    let and = OrderFilter {
        and: vec![by(FilterOp::Gte(20)), by(FilterOp::Lte(30))],
        ..Default::default()
    };
    assert_eq!(n(and).await, 2);
    // An empty sub-filter inside `or` is ignored, not "match everything".
    let or_empty = OrderFilter {
        or: vec![OrderFilter::default(), by(FilterOp::Gt(30))],
        ..Default::default()
    };
    assert_eq!(n(or_empty).await, 1);

    let (rows, total) = Order::list_and_count_where(&pool, by(FilterOp::Gte(20)), Page::new(1, 0))
        .await
        .unwrap();
    assert_eq!((rows.len(), total), (1, 3));
}

#[tokio::test]
async fn with_deleted_controls_soft_deleted_visibility_on_reads() {
    let pool = pool().await;
    let a = Order::create_one(&pool, new_order("s1@x.io", "keep"))
        .await
        .unwrap();
    let b = Order::create_one(&pool, new_order("s2@x.io", "gone"))
        .await
        .unwrap();
    Order::soft_delete(&pool, b.id.clone()).await.unwrap();

    let all = |with_deleted| OrderFilter {
        with_deleted,
        ..Default::default()
    };
    assert_eq!(Order::count_where(&pool, all(false)).await.unwrap(), 1);
    assert_eq!(Order::count_where(&pool, all(true)).await.unwrap(), 2);
    let only_deleted = OrderFilter {
        deleted_at: Some(FilterOp::IsNotNull),
        ..Default::default()
    };
    let rows = Order::list_where(&pool, only_deleted, Page::default())
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "an explicit deleted_at filter lifts the guard"
    );
    assert_eq!(rows[0].id, b.id);
    assert!(Order::get(&pool, b.id.clone()).await.unwrap().is_none());

    // restore_where is not guarded: it must reach deleted rows.
    let restored = Order::restore_where(
        &pool,
        OrderFilter {
            id: Some(FilterOp::Eq(b.id.clone())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(restored, 1);
    assert_eq!(Order::count(&pool).await.unwrap(), 2);
    let _ = a;
}

fn collect_events() -> std::sync::Arc<std::sync::Mutex<Vec<MutationEvent>>> {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = log.clone();
    flare_db::set_event_sink(move |e| sink.lock().unwrap().push(e.clone()));
    log
}

#[tokio::test]
async fn mutation_events_fire_after_success_only() {
    let pool = pool().await;
    let log = collect_events();
    let mine = |log: &std::sync::Mutex<Vec<MutationEvent>>| {
        log.lock()
            .unwrap()
            .iter()
            .filter(|e| e.object == "audited")
            .map(|e| (e.kind, e.id.clone()))
            .collect::<Vec<_>>()
    };

    let a = Audited::create_one(&pool, AuditedNew { name: "a".into() })
        .await
        .unwrap();
    let id = a.id.to_string();
    Audited::update_one(
        &pool,
        a.id,
        AuditedPatch {
            name: Some("a2".into()),
        },
    )
    .await
    .unwrap();
    Audited::soft_delete(&pool, a.id).await.unwrap();
    Audited::restore(&pool, a.id).await.unwrap();
    assert_eq!(
        mine(&log),
        [
            (MutationKind::Created, Some(id.clone())),
            (MutationKind::Updated, Some(id.clone())),
            (MutationKind::Deleted, Some(id.clone())),
            (MutationKind::Restored, Some(id.clone())),
        ]
    );

    // A failed statement emits nothing.
    let before = mine(&log).len();
    Audited::create_one(&pool, AuditedNew { name: "a2".into() })
        .await
        .must_fail();
    assert_eq!(mine(&log).len(), before);

    // Deleting a missing id is not a mutation.
    Audited::soft_delete(&pool, 9999).await.unwrap();
    assert_eq!(mine(&log).len(), before);

    // update_many emits only once its own transaction has committed.
    let b = Audited::create_one(&pool, AuditedNew { name: "b".into() })
        .await
        .unwrap();
    let before = mine(&log).len();
    let failed = Audited::update_many(
        &pool,
        vec![
            (
                b.id,
                AuditedPatch {
                    name: Some("b2".into()),
                },
            ),
            (
                b.id,
                AuditedPatch {
                    name: Some("a2".into()),
                },
            ), // unique violation
        ],
    )
    .await;
    assert!(failed.is_err());
    assert_eq!(mine(&log).len(), before, "rolled-back batch emits nothing");
    Audited::update_many(
        &pool,
        vec![(
            b.id,
            AuditedPatch {
                name: Some("b3".into()),
            },
        )],
    )
    .await
    .unwrap();
    assert_eq!(mine(&log).len(), before + 1);

    // Bulk writes report a row-count-only event.
    Audited::soft_delete_where(&pool, AuditedFilter::default())
        .await
        .unwrap();
    assert_eq!(mine(&log).last().unwrap(), &(MutationKind::Deleted, None));
}
