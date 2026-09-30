//! Live-Postgres integration test for `#[derive(Crud)]`. Gated behind the `integration-postgres`
//! feature (off by default — see flare-db/Cargo.toml) since it needs a reachable `DATABASE_URL`.
//!
//! Run with:
//! ```ignore
//! DATABASE_URL=postgres://user@localhost/db cargo test --no-default-features \
//!     --features integration-postgres --test postgres_integration
//! ```

#![cfg(feature = "integration-postgres")]

use flare_db::Crud;

#[derive(sqlx::FromRow, Crud)]
#[crud(table = "postgres_integration_posts", pk = "id", soft_delete)]
struct Post {
    id: i64,
    title: String,
    body: String,
    deleted_at: Option<time::OffsetDateTime>,
}

#[tokio::test]
async fn crud_roundtrip_against_live_postgres() {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must point at a reachable Postgres instance to run this test");
    let pool = flare_db::Pool::connect(&database_url)
        .await
        .expect("connect to postgres");

    sqlx::query("DROP TABLE IF EXISTS postgres_integration_posts")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE postgres_integration_posts (
            id BIGSERIAL PRIMARY KEY,
            title TEXT NOT NULL,
            body TEXT NOT NULL,
            deleted_at TIMESTAMPTZ
        )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let created = Post::create_one(
        &pool,
        PostNew {
            title: "hello".into(),
            body: "world".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(created.title, "hello");
    assert!(created.deleted_at.is_none());

    let fetched = Post::get(&pool, created.id).await.unwrap().unwrap();
    assert_eq!(fetched.body, "world");

    let updated = Post::update_one(
        &pool,
        created.id,
        PostPatch {
            title: Some("bye".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(updated.title, "bye");
    assert_eq!(updated.body, "world");

    assert_eq!(Post::count(&pool).await.unwrap(), 1);

    Post::soft_delete(&pool, created.id).await.unwrap();
    assert!(Post::get(&pool, created.id).await.unwrap().is_none());
    // soft-deleted rows are excluded from reads but not physically removed
    assert_eq!(Post::count(&pool).await.unwrap(), 0);
    assert_eq!(
        Post::list_and_count(&pool, flare_db::Page::default())
            .await
            .unwrap()
            .1,
        0
    );

    Post::restore(&pool, created.id).await.unwrap();
    assert!(Post::get(&pool, created.id).await.unwrap().is_some());

    let mut tx = pool.begin().await.unwrap();
    Post::create_one(
        &mut *tx,
        PostNew {
            title: "rollback".into(),
            body: "body".into(),
        },
    )
    .await
    .unwrap();
    Post::update_many(
        &mut *tx,
        vec![(
            created.id,
            PostPatch {
                title: Some("nested".into()),
                ..Default::default()
            },
        )],
    )
    .await
    .unwrap();
    assert_eq!(
        Post::list_and_count(&mut *tx, flare_db::Page::default())
            .await
            .unwrap()
            .1,
        2
    );
    tx.rollback().await.unwrap();
    assert_eq!(Post::count(&pool).await.unwrap(), 1);
    assert_eq!(
        Post::get(&pool, created.id).await.unwrap().unwrap().title,
        "bye"
    );
    assert!(
        Post::update_many(
            &pool,
            vec![
                (
                    created.id,
                    PostPatch {
                        title: Some("partial".into()),
                        ..Default::default()
                    }
                ),
                (
                    -1,
                    PostPatch {
                        title: Some("missing".into()),
                        ..Default::default()
                    }
                ),
            ]
        )
        .await
        .is_err()
    );
    assert_eq!(
        Post::get(&pool, created.id).await.unwrap().unwrap().title,
        "bye"
    );

    sqlx::query("DROP TABLE postgres_integration_posts")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[derive(sqlx::FromRow, Crud)]
#[crud(table = "postgres_integration_dal", pk = "id", unique(email))]
#[allow(dead_code)]
struct Member {
    #[crud(id(prefix = "mem"))]
    id: String,
    #[crud(immutable, searchable)]
    email: String,
    #[crud(searchable)]
    name: String,
    #[crud(hidden)]
    token: String,
    #[crud(created_at)]
    created_at: time::OffsetDateTime,
    #[crud(updated_at)]
    updated_at: time::OffsetDateTime,
}

/// Mirrors `tests/dal_features.rs` (SQLite) for the backend-specific SQL: ILIKE search with
/// escaping, ON CONFLICT upsert honouring immutable columns, SQLSTATE error mapping.
#[tokio::test]
async fn dal_features_against_live_postgres() {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must point at a reachable Postgres instance to run this test");
    let pool = flare_db::Pool::connect(&database_url).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS postgres_integration_dal")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE postgres_integration_dal (
            id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE, name TEXT NOT NULL,
            token TEXT NOT NULL, created_at TIMESTAMPTZ NOT NULL, updated_at TIMESTAMPTZ NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let new = |email: &str, name: &str| MemberNew {
        id: None,
        email: email.into(),
        name: name.into(),
        token: "t".into(),
    };
    let first = Member::upsert_one(&pool, new("a@x.io", "Ann 100%"))
        .await
        .unwrap();
    assert!(first.id.starts_with("mem_"));
    let again = Member::upsert_one(&pool, new("a@x.io", "Ann"))
        .await
        .unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(again.name, "Ann");
    assert!(again.updated_at > first.updated_at);
    Member::create_one(&pool, new("b@x.io", "Bob_2"))
        .await
        .unwrap();

    let q = |s: &str| MemberFilter {
        q: Some(s.into()),
        ..Default::default()
    };
    assert_eq!(Member::count_where(&pool, q("ANN")).await.unwrap(), 1);
    assert_eq!(Member::count_where(&pool, q("_")).await.unwrap(), 1);
    assert_eq!(Member::count_where(&pool, q("x.io")).await.unwrap(), 2);

    let err = Member::create_one(&pool, new("a@x.io", "dup"))
        .await
        .err()
        .unwrap();
    match flare_db::CrudError::from(err) {
        flare_db::CrudError::Conflict { cols, values, .. } => {
            assert_eq!(cols, ["email"]);
            assert_eq!(values, ["a@x.io"]);
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    sqlx::query("DROP TABLE postgres_integration_dal")
        .execute(&pool)
        .await
        .unwrap();
}
