use sea_query::{Condition, Expr, ExprTrait, LikeExpr, SimpleExpr};

/// One filter condition on a single column, generated per-field on `{Entity}Filter`
/// structs by the `Crud` derive. Mirrors Medusa's dynamic filter operator set.
#[derive(Debug, Clone)]
pub enum FilterOp<T> {
    Eq(T),
    Ne(T),
    In(Vec<T>),
    NotIn(Vec<T>),
    Gt(T),
    Gte(T),
    Lt(T),
    Lte(T),
    /// Inclusive on both ends.
    Between(T, T),
    /// Text-like fields only. The pattern is used as given (`%`/`_` are wildcards).
    Like(String),
    /// Case-insensitive `Like` (`ILIKE` on Postgres; ASCII-case-insensitive on SQLite).
    /// `\` escapes `%`/`_` (see [`escape_like`]).
    ILike(String),
    IsNull,
    IsNotNull,
}

/// Escapes `\`, `%` and `_` so user text matches literally inside a `LIKE` pattern.
pub fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Case-insensitive `LIKE` against `column`, with `\` as the escape character.
/// Same behaviour on both backends: Postgres uses `ILIKE`; SQLite compares
/// `lower(column)` with an ASCII-lowercased pattern (its `lower()` and default
/// `LIKE` are ASCII-only, and this does not depend on `PRAGMA case_sensitive_like`).
pub fn ilike_expr(column: &'static str, pattern: String) -> SimpleExpr {
    #[cfg(feature = "postgres")]
    {
        use sea_query::extension::postgres::PgExpr as _;
        Expr::col(column).ilike(LikeExpr::new(pattern).escape('\\'))
    }
    #[cfg(not(feature = "postgres"))]
    {
        sea_query::Func::lower(Expr::col(column))
            .like(LikeExpr::new(pattern.to_ascii_lowercase()).escape('\\'))
    }
}

/// `q` search: OR of case-insensitive substring matches of `term` over `columns`.
/// `None` for a blank term (no restriction) or no searchable columns.
pub fn search_condition(columns: &[&'static str], term: &str) -> Option<Condition> {
    let term = term.trim();
    if term.is_empty() || columns.is_empty() {
        return None;
    }
    let pattern = format!("%{}%", escape_like(term));
    Some(columns.iter().fold(Condition::any(), |c, col| {
        c.add(ilike_expr(col, pattern.clone()))
    }))
}

impl<T> FilterOp<T>
where
    T: Into<sea_query::Value>,
{
    /// Builds the sea-query expression for this operator against `column`.
    pub fn into_expr(self, column: &'static str) -> SimpleExpr {
        match self {
            FilterOp::Eq(v) => Expr::col(column).eq(v),
            FilterOp::Ne(v) => Expr::col(column).ne(v),
            FilterOp::In(vs) => Expr::col(column).is_in(vs),
            FilterOp::NotIn(vs) => Expr::col(column).is_not_in(vs),
            FilterOp::Gt(v) => Expr::col(column).gt(v),
            FilterOp::Gte(v) => Expr::col(column).gte(v),
            FilterOp::Lt(v) => Expr::col(column).lt(v),
            FilterOp::Lte(v) => Expr::col(column).lte(v),
            FilterOp::Between(a, b) => Expr::col(column).between(a, b),
            FilterOp::Like(s) => Expr::col(column).like(s),
            FilterOp::ILike(s) => ilike_expr(column, s),
            FilterOp::IsNull => Expr::col(column).is_null(),
            FilterOp::IsNotNull => Expr::col(column).is_not_null(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_query::{PostgresQueryBuilder, Query};

    fn render(expr: SimpleExpr) -> String {
        Query::select()
            .and_where(expr)
            .to_string(PostgresQueryBuilder)
    }

    #[test]
    fn eq_renders_equality() {
        assert!(render(FilterOp::Eq(1_i64).into_expr("id")).ends_with(r#"WHERE "id" = 1"#));
    }

    #[test]
    fn ne_renders_inequality() {
        assert!(render(FilterOp::Ne(1_i64).into_expr("id")).ends_with(r#"WHERE "id" <> 1"#));
    }

    #[test]
    fn in_renders_membership() {
        assert!(
            render(FilterOp::In(vec![1_i64, 2]).into_expr("id"))
                .ends_with(r#"WHERE "id" IN (1, 2)"#)
        );
    }

    #[test]
    fn not_in_renders_negated_membership() {
        assert!(
            render(FilterOp::NotIn(vec![1_i64, 2]).into_expr("id"))
                .ends_with(r#"WHERE "id" NOT IN (1, 2)"#)
        );
    }

    #[test]
    fn like_renders_pattern_match() {
        assert!(
            render(FilterOp::Like::<String>("%a%".into()).into_expr("title"))
                .ends_with(r#"WHERE "title" LIKE '%a%'"#)
        );
    }

    #[test]
    fn comparison_operators_render() {
        assert!(render(FilterOp::Gt(1_i64).into_expr("n")).ends_with(r#"WHERE "n" > 1"#));
        assert!(render(FilterOp::Gte(1_i64).into_expr("n")).ends_with(r#"WHERE "n" >= 1"#));
        assert!(render(FilterOp::Lt(1_i64).into_expr("n")).ends_with(r#"WHERE "n" < 1"#));
        assert!(render(FilterOp::Lte(1_i64).into_expr("n")).ends_with(r#"WHERE "n" <= 1"#));
        assert!(
            render(FilterOp::Between(1_i64, 5).into_expr("n"))
                .ends_with(r#"WHERE "n" BETWEEN 1 AND 5"#)
        );
    }

    #[test]
    fn escape_like_escapes_wildcards_and_backslash() {
        assert_eq!(escape_like(r"50%_off\"), r"50\%\_off\\");
    }

    #[test]
    fn search_condition_ignores_blank_terms_and_empty_columns() {
        assert!(search_condition(&["a"], "  ").is_none());
        assert!(search_condition(&[], "x").is_none());
        assert!(search_condition(&["a", "b"], "x").is_some());
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn ilike_uses_native_ilike_with_escape_on_postgres() {
        let sql = render(FilterOp::ILike::<String>("a\\_%".into()).into_expr("t"));
        assert!(
            sql.contains(r#""t" ILIKE "#) && sql.ends_with(r"ESCAPE E'\\')"),
            "{sql}"
        );
    }

    #[cfg(not(feature = "postgres"))]
    #[test]
    fn ilike_lowercases_both_sides_on_sqlite() {
        let sql = Query::select()
            .and_where(FilterOp::ILike::<String>("AbC%".into()).into_expr("t"))
            .to_string(sea_query::SqliteQueryBuilder);
        assert!(
            sql.ends_with(r#"WHERE LOWER("t") LIKE 'abc%' ESCAPE '\'"#),
            "{sql}"
        );
    }

    #[test]
    fn is_null_renders_null_check() {
        assert!(
            render(FilterOp::<i64>::IsNull.into_expr("deleted_at"))
                .ends_with(r#"WHERE "deleted_at" IS NULL"#)
        );
    }

    #[test]
    fn is_not_null_renders_negated_null_check() {
        assert!(
            render(FilterOp::<i64>::IsNotNull.into_expr("deleted_at"))
                .ends_with(r#"WHERE "deleted_at" IS NOT NULL"#)
        );
    }
}
