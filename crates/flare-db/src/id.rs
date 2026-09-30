use time::OffsetDateTime;

/// Timestamp bound for `created_at` / `updated_at` columns. Bound from Rust (not
/// `CURRENT_TIMESTAMP`) so SQLite and Postgres both keep sub-second precision.
pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

/// Field types `created_at` / `updated_at` may have (they are bound as `OffsetDateTime`).
pub trait TimestampField {}
impl TimestampField for OffsetDateTime {}
impl TimestampField for Option<OffsetDateTime> {}

/// Compile-time check used by generated code; never called.
pub fn assert_timestamp<T: TimestampField>() {}

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// `prefix_<ULID>` (Medusa `generateEntityId` style), e.g. `ord_01J8Z5...`.
///
/// # Panics
/// If the OS random source is unavailable.
pub fn generate_id(prefix: &str) -> String {
    let millis = (now().unix_timestamp_nanos() / 1_000_000) as u128;
    let mut rnd = [0u8; 10];
    getrandom::fill(&mut rnd).expect("OS random source unavailable");
    let mut n = (millis << 80) | rnd.iter().fold(0u128, |acc, b| (acc << 8) | *b as u128);
    let mut out = [0u8; 26];
    for slot in out.iter_mut().rev() {
        *slot = CROCKFORD[(n & 31) as usize];
        n >>= 5;
    }
    format!("{prefix}_{}", std::str::from_utf8(&out).expect("ascii"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_prefixed_and_unique() {
        let a = generate_id("ord");
        let b = generate_id("ord");
        assert!(a.starts_with("ord_") && a.len() == 4 + 26);
        assert_ne!(a, b);
    }
}
