//! # HTTP Cache Layer
//!
//! A caching wrapper around an HTTP client that caches GET responses in memory
//! with a configurable TTL. Only GET requests are cached; POST, PUT, DELETE, and
//! other mutating methods bypass the cache entirely.

mod cache;
mod client;
mod error;

pub use cache::{CacheConfig, CacheEntry, InMemoryCache};
pub use client::CachedHttpClient;
pub use error::CacheError;
