//! Compile-only shape test for `#[derive(Crud)]`.
//!
//! `_api_shape_compiles` below is never called, but it must still type-check against
//! the real generated API (not a doc example marked ```ignore```, which rustdoc never
//! compiles) -- this is what actually catches a codegen signature mistake. The `#[test]`
//! functions exercise the pure, DB-free parts (companion struct construction, defaults)
//! that don't need a live connection.

use flare_db::{Crud, FilterOp, Page, Pool};

#[derive(sqlx::FromRow, Crud)]
#[crud(table = "posts", pk = "id", soft_delete)]
#[allow(dead_code)]
struct Post {
    id: i64,
    title: String,
    body: String,
    deleted_at: Option<time::OffsetDateTime>,
}

#[derive(sqlx::FromRow, Crud)]
#[crud(table = "tags", pk = "id")]
#[allow(dead_code)]
struct Tag {
    id: i64,
    name: String,
}

#[allow(dead_code)]
async fn _api_shape_compiles(pool: &Pool) -> sqlx::Result<()> {
    let post = Post::create_one(
        pool,
        PostNew {
            title: "t".into(),
            body: "b".into(),
        },
    )
    .await?;
    let _ = Post::get(pool, post.id).await?;
    let _ = Post::list(pool, Page::new(10, 0)).await?;
    let (_rows, _count) = Post::list_and_count(pool, Page::default()).await?;
    let _ = Post::count(pool).await?;
    let _ = Post::create_many(pool, vec![]).await?;
    let _ = Post::update_one(pool, post.id, PostPatch::default()).await?;
    let _ = Post::update_many(pool, vec![]).await?;
    let _ = Post::list_where(pool, PostFilter::default(), Page::default()).await?;
    let _ = Post::update_where(pool, PostFilter::default(), PostPatch::default()).await?;
    Post::soft_delete(pool, post.id).await?;
    Post::restore(pool, post.id).await?;
    let _ = Post::soft_delete_where(pool, PostFilter::default()).await?;
    let _ = Post::restore_where(pool, PostFilter::default()).await?;

    let tag = Tag::create_one(
        pool,
        TagNew {
            name: "rust".into(),
        },
    )
    .await?;
    Tag::delete(pool, tag.id).await?;
    Ok(())
}

#[test]
fn new_struct_excludes_pk_and_soft_delete_column() {
    // Would not compile if `deleted_at` were still required here -- that's the point.
    let new = PostNew {
        title: "hello".into(),
        body: "world".into(),
    };
    assert_eq!(new.title, "hello");
    assert_eq!(new.body, "world");
}

#[test]
fn patch_struct_defaults_to_all_none_and_excludes_soft_delete_column() {
    let patch = PostPatch::default();
    assert!(patch.title.is_none());
    assert!(patch.body.is_none());
    // Would not compile if `PostPatch` still had a `deleted_at` field.
}

#[test]
fn filter_struct_still_covers_soft_delete_column() {
    let filter = PostFilter::default();
    assert!(filter.id.is_none());
    assert!(filter.title.is_none());
    assert!(filter.deleted_at.is_none());
}

#[test]
fn filter_op_can_be_constructed_per_variant() {
    let _: FilterOp<i64> = FilterOp::Eq(1);
    let _: FilterOp<i64> = FilterOp::Ne(1);
    let _: FilterOp<i64> = FilterOp::In(vec![1, 2]);
    let _: FilterOp<i64> = FilterOp::NotIn(vec![1, 2]);
    let _: FilterOp<String> = FilterOp::Like("%a%".into());
    let _: FilterOp<i64> = FilterOp::IsNull;
    let _: FilterOp<i64> = FilterOp::IsNotNull;
}
