use std::time::Instant;

use reqwest::{Client, Method, RequestBuilder, Response};

use crate::cache::{CacheConfig, CacheEntry, InMemoryCache};
use crate::error::CacheError;

/// An HTTP client wrapper that transparently caches GET responses in memory.
///
/// Only GET requests are cached. All other HTTP methods (POST, PUT, DELETE, PATCH,
/// etc.) bypass the cache entirely and are forwarded directly to the underlying
/// `reqwest::Client`.
///
/// # Example
///
/// ```no_run
/// use std::time::Duration;
/// use http_cache_layer::{CachedHttpClient, CacheConfig};
///
/// #[tokio::main]
/// async fn main() {
///     let client = CachedHttpClient::new(CacheConfig::with_ttl(Duration::from_secs(120)));
///
///     // First call fetches from the network.
///     let body = client.get("https://api.example.com/data").await.unwrap();
///
///     // Second call returns the cached response (within TTL).
///     let body_again = client.get("https://api.example.com/data").await.unwrap();
/// }
/// ```
#[derive(Debug, Clone)]
pub struct CachedHttpClient {
    inner: Client,
    cache: InMemoryCache,
}

/// The response returned by [`CachedHttpClient`], abstracting over both cached
/// and fresh responses.
#[derive(Debug, Clone)]
pub struct CachedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Whether this response was served from the cache.
    pub was_cached: bool,
}

impl CachedResponse {
    /// Interpret the body as a UTF-8 string.
    pub fn text(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.body)
    }

    /// Deserialize the body from JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }
}

impl CachedHttpClient {
    /// Create a new cached HTTP client with the given cache configuration.
    pub fn new(config: CacheConfig) -> Self {
        Self {
            inner: Client::new(),
            cache: InMemoryCache::new(config),
        }
    }

    /// Create a new cached HTTP client wrapping an existing `reqwest::Client`.
    pub fn with_client(client: Client, config: CacheConfig) -> Self {
        Self {
            inner: client,
            cache: InMemoryCache::new(config),
        }
    }

    /// Perform a GET request. The response is cached according to the TTL.
    pub async fn get(&self, url: &str) -> Result<CachedResponse, CacheError> {
        // Check cache first.
        if let Some(entry) = self.cache.get(url)? {
            return Ok(CachedResponse {
                status: entry.status,
                headers: entry.headers,
                body: entry.body,
                was_cached: true,
            });
        }

        // Cache miss — fetch from network.
        let response = self.inner.get(url).send().await?;
        let cached_response = Self::response_to_cached(response, false).await?;

        // Store in cache.
        let entry = CacheEntry {
            body: cached_response.body.clone(),
            status: cached_response.status,
            headers: cached_response.headers.clone(),
            inserted_at: Instant::now(),
        };
        self.cache.insert(url.to_string(), entry)?;

        Ok(cached_response)
    }

    /// Perform a POST request. This is never cached.
    pub async fn post(&self, url: &str) -> RequestBuilder {
        self.inner.post(url)
    }

    /// Perform a PUT request. This is never cached.
    pub async fn put(&self, url: &str) -> RequestBuilder {
        self.inner.put(url)
    }

    /// Perform a DELETE request. This is never cached.
    pub async fn delete(&self, url: &str) -> RequestBuilder {
        self.inner.delete(url)
    }

    /// Send an arbitrary request. Only GET requests are cached.
    pub async fn execute(
        &self,
        method: Method,
        url: &str,
    ) -> Result<CachedResponse, CacheError> {
        if method == Method::GET {
            return self.get(url).await;
        }

        // Non-GET methods are never cached.
        let response = self.inner.request(method, url).send().await?;
        Self::response_to_cached(response, false).await
    }

    /// Manually invalidate a cached URL.
    pub fn invalidate(&self, url: &str) -> Result<(), CacheError> {
        self.cache.invalidate(url)
    }

    /// Clear the entire cache.
    pub fn clear_cache(&self) -> Result<(), CacheError> {
        self.cache.clear()
    }

    /// Return a reference to the underlying cache for inspection.
    pub fn cache(&self) -> &InMemoryCache {
        &self.cache
    }

    /// Convert a `reqwest::Response` into our `CachedResponse`.
    async fn response_to_cached(
        response: Response,
        was_cached: bool,
    ) -> Result<CachedResponse, CacheError> {
        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body = response.bytes().await?.to_vec();

        Ok(CachedResponse {
            status,
            headers,
            body,
            was_cached,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_get_caches_response() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/data"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello from server"))
            .expect(1) // The mock should be called exactly once.
            .mount(&mock_server)
            .await;

        let client = CachedHttpClient::new(CacheConfig::with_ttl(Duration::from_secs(60)));
        let url = format!("{}/data", mock_server.uri());

        // First request: cache miss, hits the server.
        let resp1 = client.get(&url).await.unwrap();
        assert_eq!(resp1.status, 200, "first response status should be exactly 200");
        assert_eq!(
            resp1.text().unwrap(),
            "hello from server",
            "first response body should be exactly 'hello from server'"
        );
        assert_eq!(
            resp1.was_cached, false,
            "first response should not be from cache"
        );

        // Second request: cache hit, does NOT hit the server.
        let resp2 = client.get(&url).await.unwrap();
        assert_eq!(resp2.status, 200, "cached response status should be exactly 200");
        assert_eq!(
            resp2.text().unwrap(),
            "hello from server",
            "cached response body should be exactly 'hello from server'"
        );
        assert_eq!(
            resp2.was_cached, true,
            "second response should be served from cache"
        );

        // The mock expects exactly 1 call — if the cache didn't work, this
        // assertion (built into wiremock) would fail when the mock server drops.
    }

    #[tokio::test]
    async fn test_post_is_not_cached() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/submit"))
            .respond_with(ResponseTemplate::new(201).set_body_string("created"))
            .expect(2) // Should be called both times.
            .mount(&mock_server)
            .await;

        let client = CachedHttpClient::new(CacheConfig::with_ttl(Duration::from_secs(60)));
        let url = format!("{}/submit", mock_server.uri());

        // Send two POST requests via `execute`.
        let resp1 = client.execute(Method::POST, &url).await.unwrap();
        assert_eq!(resp1.status, 201, "first POST status should be exactly 201");
        assert_eq!(resp1.was_cached, false, "POST should never be cached");

        let resp2 = client.execute(Method::POST, &url).await.unwrap();
        assert_eq!(resp2.status, 201, "second POST status should be exactly 201");
        assert_eq!(resp2.was_cached, false, "POST should never be cached on second call either");

        // Cache should be empty — POST results are never stored.
        let cache_len = client.cache().len().unwrap();
        assert_eq!(cache_len, 0, "cache should have exactly 0 entries after POST requests");
    }

    #[tokio::test]
    async fn test_put_is_not_cached() {
        let mock_server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/update"))
            .respond_with(ResponseTemplate::new(200).set_body_string("updated"))
            .expect(2)
            .mount(&mock_server)
            .await;

        let client = CachedHttpClient::new(CacheConfig::with_ttl(Duration::from_secs(60)));
        let url = format!("{}/update", mock_server.uri());

        let resp1 = client.execute(Method::PUT, &url).await.unwrap();
        assert_eq!(resp1.status, 200, "first PUT status should be exactly 200");
        assert_eq!(resp1.was_cached, false, "PUT should never be cached");

        let resp2 = client.execute(Method::PUT, &url).await.unwrap();
        assert_eq!(resp2.status, 200, "second PUT status should be exactly 200");
        assert_eq!(resp2.was_cached, false, "PUT should never be cached on second call");

        let cache_len = client.cache().len().unwrap();
        assert_eq!(cache_len, 0, "cache should have exactly 0 entries after PUT requests");
    }

    #[tokio::test]
    async fn test_delete_is_not_cached() {
        let mock_server = MockServer::start().await;

        Mock::given(method("DELETE"))
            .and(path("/resource"))
            .respond_with(ResponseTemplate::new(204))
            .expect(2)
            .mount(&mock_server)
            .await;

        let client = CachedHttpClient::new(CacheConfig::with_ttl(Duration::from_secs(60)));
        let url = format!("{}/resource", mock_server.uri());

        let resp1 = client.execute(Method::DELETE, &url).await.unwrap();
        assert_eq!(resp1.status, 204, "first DELETE status should be exactly 204");
        assert_eq!(resp1.was_cached, false, "DELETE should never be cached");

        let resp2 = client.execute(Method::DELETE, &url).await.unwrap();
        assert_eq!(resp2.status, 204, "second DELETE status should be exactly 204");
        assert_eq!(resp2.was_cached, false, "DELETE should never be cached on second call");

        let cache_len = client.cache().len().unwrap();
        assert_eq!(cache_len, 0, "cache should have exactly 0 entries after DELETE requests");
    }

    #[tokio::test]
    async fn test_execute_get_uses_cache() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api"))
            .respond_with(ResponseTemplate::new(200).set_body_string("api data"))
            .expect(1) // Should only be called once due to caching.
            .mount(&mock_server)
            .await;

        let client = CachedHttpClient::new(CacheConfig::with_ttl(Duration::from_secs(60)));
        let url = format!("{}/api", mock_server.uri());

        let resp1 = client.execute(Method::GET, &url).await.unwrap();
        assert_eq!(resp1.was_cached, false, "first GET via execute should not be cached");
        assert_eq!(resp1.text().unwrap(), "api data", "body should be exactly 'api data'");

        let resp2 = client.execute(Method::GET, &url).await.unwrap();
        assert_eq!(resp2.was_cached, true, "second GET via execute should be served from cache");
        assert_eq!(resp2.text().unwrap(), "api data", "cached body should be exactly 'api data'");
    }

    #[tokio::test]
    async fn test_invalidate_forces_refetch() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/data"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fresh"))
            .expect(2) // Called twice: initial + after invalidation.
            .mount(&mock_server)
            .await;

        let client = CachedHttpClient::new(CacheConfig::with_ttl(Duration::from_secs(60)));
        let url = format!("{}/data", mock_server.uri());

        // Populate cache.
        let resp1 = client.get(&url).await.unwrap();
        assert_eq!(resp1.was_cached, false, "first GET should be a cache miss");

        // Verify it's cached.
        let resp2 = client.get(&url).await.unwrap();
        assert_eq!(resp2.was_cached, true, "second GET should be a cache hit");

        // Invalidate and refetch.
        client.invalidate(&url).unwrap();
        let resp3 = client.get(&url).await.unwrap();
        assert_eq!(
            resp3.was_cached, false,
            "GET after invalidation should be a cache miss"
        );
    }

    #[tokio::test]
    async fn test_different_urls_cached_independently() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(200).set_body_string("response-a"))
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/b"))
            .respond_with(ResponseTemplate::new(200).set_body_string("response-b"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = CachedHttpClient::new(CacheConfig::with_ttl(Duration::from_secs(60)));
        let url_a = format!("{}/a", mock_server.uri());
        let url_b = format!("{}/b", mock_server.uri());

        let resp_a = client.get(&url_a).await.unwrap();
        let resp_b = client.get(&url_b).await.unwrap();

        assert_eq!(resp_a.text().unwrap(), "response-a", "URL /a should return exactly 'response-a'");
        assert_eq!(resp_b.text().unwrap(), "response-b", "URL /b should return exactly 'response-b'");

        // Both should now be cached.
        let resp_a2 = client.get(&url_a).await.unwrap();
        let resp_b2 = client.get(&url_b).await.unwrap();

        assert_eq!(resp_a2.was_cached, true, "/a should be served from cache on second call");
        assert_eq!(resp_b2.was_cached, true, "/b should be served from cache on second call");
        assert_eq!(resp_a2.text().unwrap(), "response-a", "cached /a body should be exactly 'response-a'");
        assert_eq!(resp_b2.text().unwrap(), "response-b", "cached /b body should be exactly 'response-b'");

        let cache_len = client.cache().len().unwrap();
        assert_eq!(cache_len, 2, "cache should have exactly 2 entries for the two URLs");
    }

    #[tokio::test]
    async fn test_clear_cache() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/data"))
            .respond_with(ResponseTemplate::new(200).set_body_string("data"))
            .mount(&mock_server)
            .await;

        let client = CachedHttpClient::new(CacheConfig::with_ttl(Duration::from_secs(60)));
        let url = format!("{}/data", mock_server.uri());

        client.get(&url).await.unwrap();
        assert_eq!(client.cache().len().unwrap(), 1, "cache should have exactly 1 entry");

        client.clear_cache().unwrap();
        assert_eq!(
            client.cache().len().unwrap(),
            0,
            "cache should have exactly 0 entries after clear"
        );
    }

    #[tokio::test]
    async fn test_non_200_responses_are_cached() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/not-found"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .expect(1) // Only called once — the 404 is also cached.
            .mount(&mock_server)
            .await;

        let client = CachedHttpClient::new(CacheConfig::with_ttl(Duration::from_secs(60)));
        let url = format!("{}/not-found", mock_server.uri());

        let resp1 = client.get(&url).await.unwrap();
        assert_eq!(resp1.status, 404, "status should be exactly 404");
        assert_eq!(resp1.was_cached, false, "first call should be a cache miss");

        let resp2 = client.get(&url).await.unwrap();
        assert_eq!(resp2.status, 404, "cached status should be exactly 404");
        assert_eq!(resp2.was_cached, true, "second call should be a cache hit");
    }
}
