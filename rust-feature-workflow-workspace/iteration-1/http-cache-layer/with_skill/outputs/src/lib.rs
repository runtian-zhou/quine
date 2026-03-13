//! HTTP client with an optional in-memory caching layer.
//!
//! # Example
//!
//! ```rust
//! use http_cache::{CachedClient, CacheConfig, SimpleHttpClient};
//! use std::time::Duration;
//!
//! let inner = SimpleHttpClient::new();
//! let config = CacheConfig {
//!     default_ttl: Duration::from_secs(60),
//!     max_entries: Some(1000),
//! };
//! let client = CachedClient::new(inner, config);
//! // First call hits the network; second call returns from cache.
//! let resp1 = client.get("https://example.com/data").unwrap();
//! let resp2 = client.get("https://example.com/data").unwrap();
//! ```

pub mod cache;
pub mod client;
pub mod error;

pub use cache::{CacheConfig, CachedClient};
pub use client::{HttpClient, Response, SimpleHttpClient};
pub use error::HttpError;
