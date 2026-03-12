use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::error::CacheError;

/// Configuration for the in-memory cache.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Time-to-live for cached entries. After this duration, entries are
    /// considered stale and will be re-fetched on the next request.
    pub ttl: Duration,

    /// Maximum number of entries to store. When exceeded, the oldest entry
    /// is evicted. `None` means unlimited.
    pub max_entries: Option<usize>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(300), // 5 minutes
            max_entries: None,
        }
    }
}

impl CacheConfig {
    /// Create a new config with the given TTL.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            ..Default::default()
        }
    }

    /// Set the maximum number of cached entries.
    pub fn max_entries(mut self, max: usize) -> Self {
        self.max_entries = Some(max);
        self
    }
}

/// A single cached HTTP response.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// The response body stored as bytes.
    pub body: Vec<u8>,
    /// The HTTP status code of the cached response.
    pub status: u16,
    /// The response headers, stored as key-value string pairs.
    pub headers: Vec<(String, String)>,
    /// When this entry was inserted into the cache.
    pub inserted_at: Instant,
}

impl CacheEntry {
    /// Returns true if this entry has exceeded the given TTL.
    pub fn is_expired(&self, ttl: Duration) -> bool {
        self.inserted_at.elapsed() > ttl
    }
}

/// Thread-safe in-memory cache keyed by URL.
#[derive(Debug, Clone)]
pub struct InMemoryCache {
    entries: Arc<RwLock<HashMap<String, CacheEntry>>>,
    config: CacheConfig,
}

impl InMemoryCache {
    /// Create a new cache with the given configuration.
    pub fn new(config: CacheConfig) -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Look up a cached entry by URL. Returns `None` if the entry is missing
    /// or has expired (expired entries are removed on access).
    pub fn get(&self, url: &str) -> Result<Option<CacheEntry>, CacheError> {
        // First, try a read lock for the common case (cache hit, not expired).
        {
            let entries = self
                .entries
                .read()
                .map_err(|e| CacheError::LockPoisoned(e.to_string()))?;

            match entries.get(url) {
                Some(entry) if !entry.is_expired(self.config.ttl) => {
                    return Ok(Some(entry.clone()));
                }
                Some(_) => {
                    // Expired — fall through to remove it with a write lock.
                }
                None => return Ok(None),
            }
        }

        // Entry was expired; acquire write lock and remove it.
        let mut entries = self
            .entries
            .write()
            .map_err(|e| CacheError::LockPoisoned(e.to_string()))?;
        entries.remove(url);
        Ok(None)
    }

    /// Insert a response into the cache, keyed by URL.
    pub fn insert(&self, url: String, entry: CacheEntry) -> Result<(), CacheError> {
        let mut entries = self
            .entries
            .write()
            .map_err(|e| CacheError::LockPoisoned(e.to_string()))?;

        // Evict oldest entry if we've hit the max.
        if let Some(max) = self.config.max_entries {
            if entries.len() >= max && !entries.contains_key(&url) {
                if let Some(oldest_key) = entries
                    .iter()
                    .min_by_key(|(_, v)| v.inserted_at)
                    .map(|(k, _)| k.clone())
                {
                    entries.remove(&oldest_key);
                }
            }
        }

        entries.insert(url, entry);
        Ok(())
    }

    /// Remove a specific entry from the cache.
    pub fn invalidate(&self, url: &str) -> Result<(), CacheError> {
        let mut entries = self
            .entries
            .write()
            .map_err(|e| CacheError::LockPoisoned(e.to_string()))?;
        entries.remove(url);
        Ok(())
    }

    /// Remove all entries from the cache.
    pub fn clear(&self) -> Result<(), CacheError> {
        let mut entries = self
            .entries
            .write()
            .map_err(|e| CacheError::LockPoisoned(e.to_string()))?;
        entries.clear();
        Ok(())
    }

    /// Return the number of entries currently in the cache (including expired).
    pub fn len(&self) -> Result<usize, CacheError> {
        let entries = self
            .entries
            .read()
            .map_err(|e| CacheError::LockPoisoned(e.to_string()))?;
        Ok(entries.len())
    }

    /// Returns true if the cache contains no entries.
    pub fn is_empty(&self) -> Result<bool, CacheError> {
        Ok(self.len()? == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_insert_and_get() {
        let cache = InMemoryCache::new(CacheConfig::with_ttl(Duration::from_secs(60)));

        let entry = CacheEntry {
            body: b"hello world".to_vec(),
            status: 200,
            headers: vec![("content-type".to_string(), "text/plain".to_string())],
            inserted_at: Instant::now(),
        };

        cache
            .insert("https://example.com".to_string(), entry.clone())
            .unwrap();

        let cached = cache.get("https://example.com").unwrap();
        assert!(cached.is_some(), "entry should exist after insert");

        let cached = cached.unwrap();
        assert_eq!(cached.status, 200, "cached status should be exactly 200");
        assert_eq!(
            cached.body,
            b"hello world",
            "cached body should be exactly 'hello world'"
        );
        assert_eq!(
            cached.headers.len(),
            1,
            "cached headers should contain exactly 1 entry"
        );
    }

    #[test]
    fn test_cache_miss_returns_none() {
        let cache = InMemoryCache::new(CacheConfig::with_ttl(Duration::from_secs(60)));
        let result = cache.get("https://nonexistent.com").unwrap();
        assert!(
            result.is_none(),
            "cache miss should return None, not Some"
        );
    }

    #[test]
    fn test_expired_entry_returns_none() {
        // Use a TTL of 0 seconds so entries expire immediately.
        let cache = InMemoryCache::new(CacheConfig::with_ttl(Duration::from_secs(0)));

        let entry = CacheEntry {
            body: b"expired".to_vec(),
            status: 200,
            headers: vec![],
            inserted_at: Instant::now() - Duration::from_secs(1),
        };

        cache
            .insert("https://example.com".to_string(), entry)
            .unwrap();

        let result = cache.get("https://example.com").unwrap();
        assert!(
            result.is_none(),
            "expired entry should return None"
        );

        // The expired entry should have been evicted.
        let len = cache.len().unwrap();
        assert_eq!(len, 0, "cache should have exactly 0 entries after expired entry is evicted");
    }

    #[test]
    fn test_max_entries_eviction() {
        let config = CacheConfig::with_ttl(Duration::from_secs(60)).max_entries(2);
        let cache = InMemoryCache::new(config);

        // Insert two entries with staggered times so we know which is oldest.
        let entry1 = CacheEntry {
            body: b"first".to_vec(),
            status: 200,
            headers: vec![],
            inserted_at: Instant::now() - Duration::from_secs(10),
        };
        let entry2 = CacheEntry {
            body: b"second".to_vec(),
            status: 200,
            headers: vec![],
            inserted_at: Instant::now() - Duration::from_secs(5),
        };

        cache
            .insert("https://one.com".to_string(), entry1)
            .unwrap();
        cache
            .insert("https://two.com".to_string(), entry2)
            .unwrap();

        assert_eq!(
            cache.len().unwrap(),
            2,
            "cache should have exactly 2 entries before eviction"
        );

        // Insert a third entry — the oldest (one.com) should be evicted.
        let entry3 = CacheEntry {
            body: b"third".to_vec(),
            status: 200,
            headers: vec![],
            inserted_at: Instant::now(),
        };
        cache
            .insert("https://three.com".to_string(), entry3)
            .unwrap();

        assert_eq!(
            cache.len().unwrap(),
            2,
            "cache should still have exactly 2 entries after eviction"
        );
        assert!(
            cache.get("https://one.com").unwrap().is_none(),
            "oldest entry (one.com) should have been evicted"
        );
        assert!(
            cache.get("https://two.com").unwrap().is_some(),
            "second entry (two.com) should still be present"
        );
        assert!(
            cache.get("https://three.com").unwrap().is_some(),
            "newest entry (three.com) should be present"
        );
    }

    #[test]
    fn test_invalidate() {
        let cache = InMemoryCache::new(CacheConfig::with_ttl(Duration::from_secs(60)));

        let entry = CacheEntry {
            body: b"data".to_vec(),
            status: 200,
            headers: vec![],
            inserted_at: Instant::now(),
        };

        cache
            .insert("https://example.com".to_string(), entry)
            .unwrap();
        assert_eq!(cache.len().unwrap(), 1, "cache should have exactly 1 entry after insert");

        cache.invalidate("https://example.com").unwrap();
        assert_eq!(
            cache.len().unwrap(),
            0,
            "cache should have exactly 0 entries after invalidation"
        );
    }

    #[test]
    fn test_clear() {
        let cache = InMemoryCache::new(CacheConfig::with_ttl(Duration::from_secs(60)));

        for i in 0..5 {
            let entry = CacheEntry {
                body: format!("data-{i}").into_bytes(),
                status: 200,
                headers: vec![],
                inserted_at: Instant::now(),
            };
            cache
                .insert(format!("https://example.com/{i}"), entry)
                .unwrap();
        }

        assert_eq!(cache.len().unwrap(), 5, "cache should have exactly 5 entries");
        cache.clear().unwrap();
        assert_eq!(cache.len().unwrap(), 0, "cache should have exactly 0 entries after clear");
    }
}
