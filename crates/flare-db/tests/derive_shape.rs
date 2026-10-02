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

#[derive(sqlx::FromRow, Crud)]
#[crud(table = "accounts", pk = "id", soft_delete, unique(email))]
#[allow(dead_code)]
struct Account {
    #[crud(id(prefix = "acc"))]
    id: String,
    #[crud(immutable, searchable)]
    email: String,
    #[crud(readonly)]
    version: i64,
    #[crud(hidden)]
    password_hash: String,
    #[crud(computed)]
    display: String,
    #[crud(created_at)]
    created_at: time::OffsetDateTime,
    #[crud(updated_at)]
    updated_at: time::OffsetDateTime,
    #[crud(default = 0)]
    score: i64,
    deleted_at: Option<time::OffsetDateTime>,
}

#[allow(dead_code)]
async fn _policy_api_shape_compiles(pool: &Pool) -> sqlx::Result<()> {
    let row: AccountPublic = Account::create_one(
        pool,
        AccountNew {
            id: None,
            email: "a@b.c".into(),
            password_hash: "x".into(),
            score: None,
        },
    )
    .await?;
    let _: Option<AccountPartial> =
        Account::get_select(pool, row.id.clone(), &[AccountField::Email]).await?;
    let _: Vec<AccountPartial> =
        Account::list_select(pool, AccountFilter::default(), Page::default(), &[]).await?;
    let _: Vec<AccountPublic> =
        Account::upsert_many_on(pool, &[AccountField::Email], vec![]).await?;
    let _: Vec<AccountPublic> = Account::upsert_many(pool, vec![]).await?;
    let _: i64 = Account::count_where(
        pool,
        AccountFilter {
            q: Some("a".into()),
            ..Default::default()
        },
    )
    .await?;
    let _ = flare_db::CrudError::from(sqlx::Error::RowNotFound);
    Ok(())
}

#[test]
fn policy_attributes_shape_new_and_patch() {
    // Exhaustive (no `..`): readonly/computed/auto fields are not in New; id and
    // default fields are optional.
    let AccountNew {
        id: _,
        email: _,
        password_hash: _,
        score: _,
    } = AccountNew {
        id: None,
        email: String::new(),
        password_hash: String::new(),
        score: None,
    };
    // Patch: no id (pk), email (immutable), version (readonly), timestamps, deleted_at.
    let AccountPatch {
        password_hash: _,
        score: _,
    } = AccountPatch::default();
    assert_eq!(
        Account::COLUMNS,
        [
            "id",
            "email",
            "version",
            "created_at",
            "updated_at",
            "score",
            "deleted_at"
        ]
    );
    assert_eq!(AccountField::Email.as_str(), "email");
}
