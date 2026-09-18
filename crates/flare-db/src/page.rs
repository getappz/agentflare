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
}
