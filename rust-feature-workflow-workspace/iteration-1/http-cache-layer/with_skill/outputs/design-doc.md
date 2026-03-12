# Design: HTTP Client Caching Layer

## Status
Implemented

## Summary
Add an in-memory caching layer for the HTTP client that caches GET responses with a configurable time-to-live (TTL). Cache misses transparently fetch from the network. Non-idempotent methods (POST, PUT, DELETE) bypass the cache entirely and also invalidate any cached entry for the same URL.

## Motivation
Repeated GET requests to the same endpoint are a common performance bottleneck. A caching layer reduces latency and network usage for read-heavy workloads without requiring callers to manage their own cache. Making the TTL configurable lets consumers tune freshness vs. performance for their specific use case.

## Design

### Public API Changes

A new module `cache` is introduced, exposing:

- **`CacheConfig`** -- Configuration struct with:
  - `default_ttl: Duration` -- how long entries live before becoming stale.
  - `max_entries: usize` -- optional upper bound on cached entries (evicts oldest on overflow).

- **`CachedClient`** -- Wraps any type that implements the `HttpClient` trait (the project's existing HTTP abstraction). It exposes the same `HttpClient` interface so it is a drop-in replacement.
  - `CachedClient::new(inner: impl HttpClient, config: CacheConfig) -> Self`
  - `CachedClient::invalidate(url: &str)` -- manually evict an entry.
  - `CachedClient::clear()` -- flush the entire cache.

- **`HttpClient` trait** (assumed existing or introduced here) -- the minimal trait the cache decorates:
  - `fn get(&self, url: &str) -> Result<Response, HttpError>`
  - `fn post(&self, url: &str, body: &[u8]) -> Result<Response, HttpError>`
  - `fn put(&self, url: &str, body: &[u8]) -> Result<Response, HttpError>`
  - `fn delete(&self, url: &str) -> Result<Response, HttpError>`

`CachedClient` implements `HttpClient`:
- `get` -- checks cache first; on miss, delegates to `inner.get()` and stores the result.
- `post` / `put` / `delete` -- always delegates to inner, then invalidates any cached entry for that URL.

### Internal Design

```
CachedClient
  +-- inner: Box<dyn HttpClient>
  +-- cache: HashMap<String, CacheEntry>
  +-- config: CacheConfig

CacheEntry
  +-- response: Response
  +-- inserted_at: Instant
```

Key decisions:
1. **HashMap keyed by URL string.** Simple and sufficient for the initial version. Future work could add Vary-header-aware keys.
2. **TTL checked on read.** Expired entries are lazily evicted when accessed. A background reaper is out of scope for this iteration.
3. **`max_entries` eviction.** When the cache is full, the entry with the oldest `inserted_at` is removed before inserting a new one. This is O(n) per eviction but acceptable for moderate cache sizes.
4. **Thread safety.** The cache is wrapped in `Arc<Mutex<...>>` so `CachedClient` is `Send + Sync`. Callers using async runtimes should note that the mutex is a `std::sync::Mutex` (not tokio), kept briefly locked, which avoids introducing an async runtime dependency.

### Error Handling

- Network errors from the inner client are propagated as-is; they are never cached.
- If serialization/deserialization of cached responses fails (shouldn't happen with in-memory clones), the error is logged and the cache entry is evicted, falling back to a fresh network request.

## Alternatives Considered

1. **HTTP-header-driven caching (Cache-Control, ETag, etc.).**
   Rejected for the initial version because it adds significant complexity (parsing headers, conditional requests) and the user specifically asked for a simple TTL-based approach. This could be layered on top later.

2. **LRU cache via `lru` crate.**
   A good option, but introduces an external dependency. For the initial version we use a simple HashMap + oldest-eviction to keep dependencies minimal. Switching to `lru` later is a one-struct swap.

3. **Caching at the response-body bytes level only.**
   Rejected because callers need the full `Response` (status code, headers), not just the body.

## Testing Plan

### Unit tests (`src/cache.rs` -- `#[cfg(test)]`)
1. **Cache hit** -- GET the same URL twice; second call returns cached data without hitting the inner client.
2. **Cache miss** -- GET a URL not in the cache; inner client is called.
3. **TTL expiry** -- Insert an entry, advance time past TTL, verify next GET is a cache miss.
4. **POST/PUT/DELETE bypass** -- These methods always hit the inner client.
5. **POST/PUT/DELETE invalidation** -- A cached GET entry is evicted after a POST to the same URL.
6. **`max_entries` eviction** -- Fill the cache to capacity, insert one more, verify the oldest entry was evicted.
7. **Manual `invalidate` and `clear`** -- Verify they remove the expected entries.
8. **Network errors are not cached** -- If inner.get() returns Err, subsequent GET should try the network again.

### Integration tests (`tests/cached_client_integration.rs`)
1. End-to-end test with a mock HTTP server (e.g., `mockito` or hand-rolled mock) verifying request counts.

## Unresolved Questions
1. Should the cache key include query parameters? (Current answer: yes, the full URL string is the key.)
2. Should non-200 responses be cached? (Current answer: only 2xx responses are cached; errors and redirects are not.)
