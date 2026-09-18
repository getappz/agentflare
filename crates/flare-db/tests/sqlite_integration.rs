//! Live in-memory SQLite integration test for `#[derive(Crud)]`. Runs under the default
//! `sqlite` feature — no external service needed, unlike `postgres_integration.rs` — and
//! also exercises `run_migrations` against this crate's own `migrations/0001_posts.sql`.

#![cfg(feature = "sqlite")]

use flare_db::Crud;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(sqlx::FromRow, Crud)]
#[crud(table = "posts", pk = "id", soft_delete)]
struct Post {
    id: i64,
    title: String,
    body: String,
    deleted_at: Option<time::OffsetDateTime>,
}

#[derive(sqlx::FromRow, Crud)]
#[crud(table = "tags", pk = "id")]
struct Tag {
    id: i64,
    name: String,
}

#[tokio::test]
async fn crud_roundtrip_against_in_memory_sqlite() {
    let pool = flare_db::Pool::connect("sqlite::memory:")
        .await
        .expect("connect to in-memory sqlite");
    flare_db::run_migrations(&pool, &MIGRATOR)
        .await
        .expect("run migrations");

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

    let (rows, total) = Post::list_and_count(&pool, flare_db::Page::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(total, 1);

    let filtered = Post::list_where(
        &pool,
        PostFilter {
            title: Some(flare_db::FilterOp::Eq("bye".into())),
            ..Default::default()
        },
        flare_db::Page::default(),
    )
    .await
    .unwrap();
    assert_eq!(filtered.len(), 1);

    Post::soft_delete(&pool, created.id).await.unwrap();
    assert!(Post::get(&pool, created.id).await.unwrap().is_none());
    // soft-deleted rows are excluded from reads but not physically removed
    assert_eq!(Post::count(&pool).await.unwrap(), 0);

    Post::restore(&pool, created.id).await.unwrap();
    assert!(Post::get(&pool, created.id).await.unwrap().is_some());

    let tag = Tag::create_one(
        &pool,
        TagNew {
            name: "rust".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(tag.name, "rust");
    assert_eq!(Tag::count(&pool).await.unwrap(), 1);
    Tag::delete(&pool, tag.id).await.unwrap();
    assert_eq!(Tag::count(&pool).await.unwrap(), 0);
}
