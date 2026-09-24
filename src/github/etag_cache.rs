//! A bounded, per-process cache of GET responses keyed by URL, holding each
//! response's `ETag` and body so `Client` can send `If-None-Match` and
//! serve the cached body when GitHub answers 304. A 304 costs no primary
//! rate-limit budget, which is what makes polling the same PRs and issues
//! every sweep tick affordable: an unchanged resource is a header exchange,
//! not a re-download that counts against the 5,000/hour allowance.
//!
//! Bounded two ways: a fixed number of entries, evicted oldest-first, and a
//! cap on the size of any one cached body, so a single huge list page can't
//! pin megabytes for the life of the daemon.

use std::collections::{HashMap, VecDeque};

/// Entries kept per host. A sweep over a few hundred PRs across a dozen
/// projects touches a few hundred distinct URLs; this leaves room for the
/// bridge's issue polling on top without growing without bound.
pub(crate) const MAX_ENTRIES: usize = 1024;

/// Bodies above this are not cached at all -- a 100-item list page of PRs
/// with full bodies is well under it, and anything bigger is rare enough
/// that re-downloading it beats holding it.
pub(crate) const MAX_BODY_BYTES: usize = 512 * 1024;

/// One cached GET: the validator to send back, and the body it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CachedGet {
    pub etag: String,
    pub body: String,
}

#[derive(Debug, Default)]
pub(crate) struct EtagCache {
    entries: HashMap<String, CachedGet>,
    /// Insertion order, for oldest-first eviction. A re-inserted key keeps
    /// its original position: an entry that keeps changing is not one worth
    /// keeping longer, and a plain FIFO is enough for a cache whose only job
    /// is to stay bounded.
    order: VecDeque<String>,
}

impl EtagCache {
    pub(crate) fn get(&self, key: &str) -> Option<&CachedGet> {
        self.entries.get(key)
    }

    /// Stores `body` under `key` with its validator, or drops any existing
    /// entry for `key` when the body is too big to keep -- a stale validator
    /// must never outlive the body it validates.
    pub(crate) fn insert(&mut self, key: &str, etag: &str, body: &str) {
        if body.len() > MAX_BODY_BYTES {
            self.remove(key);
            return;
        }
        if self.entries.contains_key(key) {
            // Keep its slot in `order`; only the payload changes.
        } else {
            self.order.push_back(key.to_string());
            while self.order.len() > MAX_ENTRIES {
                if let Some(oldest) = self.order.pop_front() {
                    self.entries.remove(&oldest);
                }
            }
        }
        self.entries.insert(
            key.to_string(),
            CachedGet {
                etag: etag.to_string(),
                body: body.to_string(),
            },
        );
    }

    pub(crate) fn remove(&mut self, key: &str) {
        if self.entries.remove(key).is_some() {
            self.order.retain(|k| k != key);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_get_round_trips_and_replaces_in_place() {
        let mut cache = EtagCache::default();
        cache.insert("k", "\"v1\"", "{\"a\":1}");
        assert_eq!(cache.get("k").unwrap().etag, "\"v1\"");
        cache.insert("k", "\"v2\"", "{\"a\":2}");
        assert_eq!(cache.get("k").unwrap().body, "{\"a\":2}");
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.order.len(), 1, "a replaced key keeps one slot");
    }

    #[test]
    fn the_oldest_entry_is_evicted_past_the_entry_cap() {
        let mut cache = EtagCache::default();
        for i in 0..=MAX_ENTRIES {
            cache.insert(&format!("k{i}"), "\"e\"", "{}");
        }
        assert_eq!(cache.len(), MAX_ENTRIES);
        assert!(cache.get("k0").is_none(), "the first entry goes first");
        assert!(cache.get(&format!("k{MAX_ENTRIES}")).is_some());
    }

    #[test]
    fn an_oversized_body_is_not_cached_and_drops_a_stale_entry() {
        let mut cache = EtagCache::default();
        cache.insert("k", "\"v1\"", "{}");
        let huge = "x".repeat(MAX_BODY_BYTES + 1);
        cache.insert("k", "\"v2\"", &huge);
        assert!(
            cache.get("k").is_none(),
            "a validator without its body would make a 304 unservable"
        );
        assert!(cache.order.is_empty());
    }

    #[test]
    fn remove_drops_the_entry_and_its_slot() {
        let mut cache = EtagCache::default();
        cache.insert("a", "\"1\"", "{}");
        cache.insert("b", "\"2\"", "{}");
        cache.remove("a");
        assert!(cache.get("a").is_none());
        assert_eq!(cache.order, VecDeque::from(vec!["b".to_string()]));
        // Removing an absent key is a no-op.
        cache.remove("zzz");
        assert_eq!(cache.len(), 1);
    }
}
