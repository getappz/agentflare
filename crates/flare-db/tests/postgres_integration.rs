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

    Post::restore(&pool, created.id).await.unwrap();
    assert!(Post::get(&pool, created.id).await.unwrap().is_some());

    sqlx::query("DROP TABLE postgres_integration_posts")
        .execute(&pool)
        .await
        .unwrap();
}
