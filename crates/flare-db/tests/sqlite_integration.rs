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
    assert_eq!(
        Post::list_and_count(&pool, flare_db::Page::default())
            .await
            .unwrap()
            .1,
        0
    );

    Post::restore(&pool, created.id).await.unwrap();
    assert!(Post::get(&pool, created.id).await.unwrap().is_some());

    let tag = Tag::create_one(
        &pool,
        TagNew {
            name: "rust'); DROP TABLE posts; --".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(tag.name, "rust'); DROP TABLE posts; --");
    assert_eq!(
        Tag::list_where(
            &pool,
            TagFilter {
                name: Some(flare_db::FilterOp::Eq(tag.name.clone())),
                ..Default::default()
            },
            flare_db::Page::default(),
        )
        .await
        .unwrap()
        .len(),
        1
    );
    assert_eq!(Post::count(&pool).await.unwrap(), 1);
    assert_eq!(Tag::count(&pool).await.unwrap(), 1);
    Tag::delete(&pool, tag.id).await.unwrap();
    assert_eq!(Tag::count(&pool).await.unwrap(), 0);
}

#[tokio::test]
async fn caller_transaction_and_failed_batch_roll_back() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(any(
        feature = "sqlcipher-external",
        feature = "sqlcipher-bundled",
        feature = "sqlcipher-bundled-external-openssl"
    ))]
    let pool = flare_db::connect_encrypted_sqlite(dir.path().join("atomic.db"), &[42; 32], true)
        .await
        .unwrap();
    #[cfg(not(any(
        feature = "sqlcipher-external",
        feature = "sqlcipher-bundled",
        feature = "sqlcipher-bundled-external-openssl"
    )))]
    let pool = {
        let _ = &dir;
        flare_db::Pool::connect("sqlite::memory:").await.unwrap()
    };
    flare_db::run_migrations(&pool, &MIGRATOR).await.unwrap();
    let original = Post::create_one(
        &pool,
        PostNew {
            title: "original".into(),
            body: "body".into(),
        },
    )
    .await
    .unwrap();

    // A late failure must roll back earlier updates, including without a caller tx.
    assert!(
        Post::update_many(
            &pool,
            vec![
                (
                    original.id,
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
        Post::get(&pool, original.id).await.unwrap().unwrap().title,
        "original"
    );

    let mut tx = pool.begin().await.unwrap();
    assert!(
        Post::update_many(
            &mut *tx,
            vec![
                (
                    original.id,
                    PostPatch {
                        title: Some("savepoint partial".into()),
                        ..Default::default()
                    },
                ),
                (-1, PostPatch::default()),
            ],
        )
        .await
        .is_err()
    );
    assert_eq!(
        Post::get(&mut *tx, original.id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "original"
    );
    let added = Post::create_one(
        &mut *tx,
        PostNew {
            title: "transaction".into(),
            body: "body".into(),
        },
    )
    .await
    .unwrap();
    Post::update_one(&mut *tx, added.id, PostPatch::default())
        .await
        .unwrap();
    Post::update_many(
        &mut *tx,
        vec![(
            added.id,
            PostPatch {
                title: Some("updated".into()),
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
    Post::soft_delete(&mut *tx, added.id).await.unwrap();
    assert_eq!(Post::count(&mut *tx).await.unwrap(), 1);
    Post::restore(&mut *tx, added.id).await.unwrap();
    let tag = Tag::create_one(
        &mut *tx,
        TagNew {
            name: "audit".into(),
        },
    )
    .await
    .unwrap();
    Tag::delete(&mut *tx, tag.id).await.unwrap();
    // Simulate a later application statement failing in an invoice/payment/audit unit.
    assert!(
        sqlx::query("INSERT INTO posts (id, title, body) VALUES (?, 'duplicate', 'body')")
            .bind(original.id)
            .execute(&mut *tx)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    assert_eq!(Post::count(&pool).await.unwrap(), 1);
    assert_eq!(Tag::count(&pool).await.unwrap(), 0);

    let mut tx = pool.begin().await.unwrap();
    Post::create_one(
        &mut *tx,
        PostNew {
            title: "drop rollback".into(),
            body: "body".into(),
        },
    )
    .await
    .unwrap();
    drop(tx);
    assert_eq!(Post::count(&pool).await.unwrap(), 1);

    let mut tx = pool.begin().await.unwrap();
    Post::update_many(
        &mut *tx,
        vec![(
            original.id,
            PostPatch {
                title: Some("committed".into()),
                ..Default::default()
            },
        )],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        Post::get(&pool, original.id).await.unwrap().unwrap().title,
        "committed"
    );
    pool.close().await;
}
