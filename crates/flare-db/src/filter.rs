use sea_query::{Expr, SimpleExpr};

/// One filter condition on a single column, generated per-field on `{Entity}Filter`
/// structs by the `Crud` derive. Mirrors Medusa's dynamic filter operator set.
#[derive(Debug, Clone)]
pub enum FilterOp<T> {
    Eq(T),
    Ne(T),
    In(Vec<T>),
    NotIn(Vec<T>),
    /// Text-like fields only.
    Like(String),
    IsNull,
    IsNotNull,
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
            FilterOp::Like(s) => Expr::col(column).like(s),
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
