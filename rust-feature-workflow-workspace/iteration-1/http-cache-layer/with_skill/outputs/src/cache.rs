use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::client::{HttpClient, Response};
use crate::error::HttpError;

/// Configuration for the caching layer.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// How long a cached response is considered fresh.
    pub default_ttl: Duration,
    /// Maximum number of entries in the cache. `None` means unlimited.
    pub max_entries: Option<usize>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            default_ttl: Duration::from_secs(300),
            max_entries: None,
        }
    }
}

/// A single cached response and its metadata.
#[derive(Debug, Clone)]
struct CacheEntry {
    response: Response,
    inserted_at: Instant,
}

/// Shared, mutable cache state behind a mutex.
#[derive(Debug)]
struct CacheState {
    entries: HashMap<String, CacheEntry>,
}

/// An HTTP client wrapper that caches GET responses in memory.
///
/// - **GET** requests are served from cache when a fresh entry exists.
/// - **POST / PUT / DELETE** requests always go to the network and
///   invalidate any cached entry for the same URL.
///
/// `CachedClient` implements [`HttpClient`], so it is a drop-in replacement
/// for any inner client.
#[derive(Debug)]
pub struct CachedClient<C: HttpClient> {
    inner: C,
    state: Arc<Mutex<CacheState>>,
    config: CacheConfig,
}

impl<C: HttpClient> CachedClient<C> {
    /// Create a new caching wrapper around `inner`.
    pub fn new(inner: C, config: CacheConfig) -> Self {
        Self {
            inner,
            state: Arc::new(Mutex::new(CacheState {
                entries: HashMap::new(),
            })),
            config,
        }
    }

    /// Manually invalidate (evict) the cached entry for `url`.
    pub fn invalidate(&self, url: &str) {
        let mut state = self.state.lock().expect("cache lock poisoned");
        state.entries.remove(url);
    }

    /// Remove all entries from the cache.
    pub fn clear(&self) {
        let mut state = self.state.lock().expect("cache lock poisoned");
        state.entries.clear();
    }

    /// Try to get a fresh entry from the cache.
    fn get_cached(&self, url: &str) -> Option<Response> {
        let mut state = self.state.lock().expect("cache lock poisoned");
        if let Some(entry) = state.entries.get(url) {
            if entry.inserted_at.elapsed() < self.config.default_ttl {
                return Some(entry.response.clone());
            }
            // Entry is stale -- remove it.
            state.entries.remove(url);
        }
        None
    }

    /// Store a response in the cache, evicting the oldest entry if at capacity.
    fn store(&self, url: String, response: &Response) {
        let mut state = self.state.lock().expect("cache lock poisoned");

        // Evict oldest entry if we are at capacity.
        if let Some(max) = self.config.max_entries {
            if state.entries.len() >= max && !state.entries.contains_key(&url) {
                // Find the oldest entry by inserted_at.
                if let Some(oldest_key) = state
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.inserted_at)
                    .map(|(k, _)| k.clone())
                {
                    state.entries.remove(&oldest_key);
                }
            }
        }

        state.entries.insert(
            url,
            CacheEntry {
                response: response.clone(),
                inserted_at: Instant::now(),
            },
        );
    }

    /// Returns true if `status` represents a cacheable success response.
    fn is_cacheable_status(status: u16) -> bool {
        (200..300).contains(&status)
    }
}

impl<C: HttpClient> HttpClient for CachedClient<C> {
    fn get(&self, url: &str) -> Result<Response, HttpError> {
        // Check cache first.
        if let Some(cached) = self.get_cached(url) {
            return Ok(cached);
        }

        // Cache miss -- fetch from network.
        let response = self.inner.get(url)?;

        // Only cache successful responses.
        if Self::is_cacheable_status(response.status) {
            self.store(url.to_string(), &response);
        }

        Ok(response)
    }

    fn post(&self, url: &str, body: &[u8]) -> Result<Response, HttpError> {
        let response = self.inner.post(url, body)?;
        self.invalidate(url);
        Ok(response)
    }

    fn put(&self, url: &str, body: &[u8]) -> Result<Response, HttpError> {
        let response = self.inner.put(url, body)?;
        self.invalidate(url);
        Ok(response)
    }

    fn delete(&self, url: &str) -> Result<Response, HttpError> {
        let response = self.inner.delete(url)?;
        self.invalidate(url);
        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A mock HTTP client that counts how many times each method is called
    /// and returns a configurable response.
    struct MockClient {
        get_count: AtomicU32,
        post_count: AtomicU32,
        put_count: AtomicU32,
        delete_count: AtomicU32,
        /// If set, `get` will return this error instead of a successful response.
        get_error: Mutex<Option<HttpError>>,
    }

    impl MockClient {
        fn new() -> Self {
            Self {
                get_count: AtomicU32::new(0),
                post_count: AtomicU32::new(0),
                put_count: AtomicU32::new(0),
                delete_count: AtomicU32::new(0),
                get_error: Mutex::new(None),
            }
        }

        fn get_call_count(&self) -> u32 {
            self.get_count.load(Ordering::SeqCst)
        }

        fn post_call_count(&self) -> u32 {
            self.post_count.load(Ordering::SeqCst)
        }

        fn put_call_count(&self) -> u32 {
            self.put_count.load(Ordering::SeqCst)
        }

        fn delete_call_count(&self) -> u32 {
            self.delete_count.load(Ordering::SeqCst)
        }

        fn set_get_error(&self, err: HttpError) {
            *self.get_error.lock().unwrap() = Some(err);
        }

        fn clear_get_error(&self) {
            *self.get_error.lock().unwrap() = None;
        }

        fn make_response(body: &str) -> Response {
            Response {
                status: 200,
                headers: vec![("content-type".into(), "text/plain".into())],
                body: body.as_bytes().to_vec(),
            }
        }
    }

    impl HttpClient for MockClient {
        fn get(&self, url: &str) -> Result<Response, HttpError> {
            self.get_count.fetch_add(1, Ordering::SeqCst);
            if let Some(err) = self.get_error.lock().unwrap().clone() {
                return Err(err);
            }
            Ok(Self::make_response(&format!("GET {url}")))
        }

        fn post(&self, url: &str, _body: &[u8]) -> Result<Response, HttpError> {
            self.post_count.fetch_add(1, Ordering::SeqCst);
            Ok(Self::make_response(&format!("POST {url}")))
        }

        fn put(&self, url: &str, _body: &[u8]) -> Result<Response, HttpError> {
            self.put_count.fetch_add(1, Ordering::SeqCst);
            Ok(Self::make_response(&format!("PUT {url}")))
        }

        fn delete(&self, url: &str) -> Result<Response, HttpError> {
            self.delete_count.fetch_add(1, Ordering::SeqCst);
            Ok(Self::make_response(&format!("DELETE {url}")))
        }
    }

    fn make_cached_client(ttl_secs: u64, max_entries: Option<usize>) -> CachedClient<MockClient> {
        let config = CacheConfig {
            default_ttl: Duration::from_secs(ttl_secs),
            max_entries,
        };
        CachedClient::new(MockClient::new(), config)
    }

    // -----------------------------------------------------------------------
    // Test 1: Cache hit -- second GET returns cached data without network call
    // -----------------------------------------------------------------------
    #[test]
    fn test_cache_hit_returns_cached_response() {
        let client = make_cached_client(60, None);

        let resp1 = client.get("https://example.com/data").unwrap();
        let resp2 = client.get("https://example.com/data").unwrap();

        // The inner client should have been called exactly once because the
        // second request is served from the cache.
        assert_eq!(client.inner.get_call_count(), 1, "inner GET called exactly once");
        // Both responses must be identical.
        assert_eq!(resp1, resp2, "cached response matches original");
    }

    // -----------------------------------------------------------------------
    // Test 2: Cache miss -- different URLs each trigger a network call
    // -----------------------------------------------------------------------
    #[test]
    fn test_cache_miss_hits_network() {
        let client = make_cached_client(60, None);

        let _r1 = client.get("https://example.com/a").unwrap();
        let _r2 = client.get("https://example.com/b").unwrap();

        // Two distinct URLs means two network calls.
        assert_eq!(client.inner.get_call_count(), 2, "each unique URL triggers a network call");
    }

    // -----------------------------------------------------------------------
    // Test 3: TTL expiry -- stale entry causes a cache miss
    // -----------------------------------------------------------------------
    #[test]
    fn test_ttl_expiry_causes_cache_miss() {
        // Use a very short TTL so the entry expires quickly.
        let config = CacheConfig {
            default_ttl: Duration::from_millis(1),
            max_entries: None,
        };
        let client = CachedClient::new(MockClient::new(), config);

        let _r1 = client.get("https://example.com/data").unwrap();

        // Sleep just long enough for the TTL to expire.
        std::thread::sleep(Duration::from_millis(10));

        let _r2 = client.get("https://example.com/data").unwrap();

        // After TTL expiry the inner client should be called a second time.
        assert_eq!(client.inner.get_call_count(), 2, "expired entry triggers a fresh network call");
    }

    // -----------------------------------------------------------------------
    // Test 4: POST/PUT/DELETE always bypass cache
    // -----------------------------------------------------------------------
    #[test]
    fn test_mutating_methods_always_hit_network() {
        let client = make_cached_client(60, None);

        client.post("https://example.com/x", b"body").unwrap();
        client.post("https://example.com/x", b"body").unwrap();
        assert_eq!(client.inner.post_call_count(), 2, "POST always hits network");

        client.put("https://example.com/x", b"body").unwrap();
        client.put("https://example.com/x", b"body").unwrap();
        assert_eq!(client.inner.put_call_count(), 2, "PUT always hits network");

        client.delete("https://example.com/x").unwrap();
        client.delete("https://example.com/x").unwrap();
        assert_eq!(client.inner.delete_call_count(), 2, "DELETE always hits network");
    }

    // -----------------------------------------------------------------------
    // Test 5: POST invalidates a cached GET for the same URL
    // -----------------------------------------------------------------------
    #[test]
    fn test_post_invalidates_cached_get() {
        let client = make_cached_client(60, None);

        // Populate cache.
        let _r1 = client.get("https://example.com/resource").unwrap();
        assert_eq!(client.inner.get_call_count(), 1);

        // POST to the same URL should invalidate the cache entry.
        client.post("https://example.com/resource", b"update").unwrap();

        // Next GET should be a cache miss.
        let _r2 = client.get("https://example.com/resource").unwrap();
        assert_eq!(
            client.inner.get_call_count(),
            2,
            "GET after POST to same URL must hit network"
        );
    }

    // -----------------------------------------------------------------------
    // Test 6: max_entries eviction -- oldest entry is evicted at capacity
    // -----------------------------------------------------------------------
    #[test]
    fn test_max_entries_eviction() {
        let client = make_cached_client(60, Some(2));

        // Fill the cache to capacity with 2 entries.
        client.get("https://example.com/first").unwrap();
        client.get("https://example.com/second").unwrap();
        assert_eq!(client.inner.get_call_count(), 2);

        // Adding a third entry should evict the oldest ("first").
        client.get("https://example.com/third").unwrap();
        assert_eq!(client.inner.get_call_count(), 3);

        // "second" should still be cached (no new network call).
        client.get("https://example.com/second").unwrap();
        assert_eq!(client.inner.get_call_count(), 3, "second entry is still cached");

        // "first" was evicted, so this should be a cache miss.
        client.get("https://example.com/first").unwrap();
        assert_eq!(
            client.inner.get_call_count(),
            4,
            "evicted entry triggers a network call"
        );
    }

    // -----------------------------------------------------------------------
    // Test 7: Manual invalidate and clear
    // -----------------------------------------------------------------------
    #[test]
    fn test_invalidate_removes_single_entry() {
        let client = make_cached_client(60, None);

        client.get("https://example.com/a").unwrap();
        client.get("https://example.com/b").unwrap();
        assert_eq!(client.inner.get_call_count(), 2);

        // Invalidate only "a".
        client.invalidate("https://example.com/a");

        // "a" is a miss now, "b" is still cached.
        client.get("https://example.com/a").unwrap();
        client.get("https://example.com/b").unwrap();
        assert_eq!(
            client.inner.get_call_count(),
            3,
            "only the invalidated URL triggers a new network call"
        );
    }

    #[test]
    fn test_clear_removes_all_entries() {
        let client = make_cached_client(60, None);

        client.get("https://example.com/a").unwrap();
        client.get("https://example.com/b").unwrap();
        assert_eq!(client.inner.get_call_count(), 2);

        client.clear();

        // Both should now be cache misses.
        client.get("https://example.com/a").unwrap();
        client.get("https://example.com/b").unwrap();
        assert_eq!(
            client.inner.get_call_count(),
            4,
            "all entries cleared, both URLs are cache misses"
        );
    }

    // -----------------------------------------------------------------------
    // Test 8: Network errors are not cached
    // -----------------------------------------------------------------------
    #[test]
    fn test_network_errors_are_not_cached() {
        let client = make_cached_client(60, None);

        // Make the inner client return an error.
        client
            .inner
            .set_get_error(HttpError::Network("connection refused".into()));

        let result = client.get("https://example.com/fail");
        assert!(result.is_err(), "error is propagated to caller");
        assert_eq!(client.inner.get_call_count(), 1);

        // Clear the error -- next call should succeed and hit the network.
        client.inner.clear_get_error();

        let result = client.get("https://example.com/fail");
        assert!(result.is_ok(), "succeeds after error is cleared");
        assert_eq!(
            client.inner.get_call_count(),
            2,
            "error was not cached, so network is tried again"
        );
    }
}
