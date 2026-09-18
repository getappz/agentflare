/// Pagination input for `list`/`list_and_count`/`list_where`.
///
/// `skip` defaults to 0; `take` is unbounded when `None` — mirrors Medusa's own
/// default (a known footgun there for unbounded fetches: HTTP-layer callers should
/// always pass `take`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Page {
    pub take: Option<u32>,
    pub skip: u32,
}

impl Page {
    pub fn new(take: u32, skip: u32) -> Self {
        Self {
            take: Some(take),
            skip,
        }
    }

    /// Applies this page to a sea-query `SELECT`, generated-code-side.
    ///
    /// Unlike Postgres, SQLite rejects a bare `OFFSET` with no preceding `LIMIT`
    /// (`near "OFFSET": syntax error`) -- sea-query itself renders whatever `.offset()`/
    /// `.limit()` calls it's given with no backend-specific adjustment. So an offset is
    /// only emitted paired with a limit, substituting `i64::MAX` as "no cap" when `take`
    /// is `None`, keeping the two backends' generated SQL behaviorally identical.
    pub fn apply(&self, q: &mut sea_query::SelectStatement) {
        if self.skip > 0 {
            q.limit(self.take.map_or(i64::MAX as u64, |take| take as u64));
            q.offset(self.skip as u64);
        } else if let Some(take) = self.take {
            q.limit(take as u64);
        }
    }
}
