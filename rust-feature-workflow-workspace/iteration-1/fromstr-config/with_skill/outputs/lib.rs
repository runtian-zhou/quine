//! A simple key-value configuration library.
//!
//! Parse configuration from a comma-separated `key=value` string:
//!
//! ```
//! use config::Config;
//!
//! let cfg: Config = "host=localhost,port=8080".parse().unwrap();
//! assert_eq!(cfg.get("host"), Some("localhost"));
//! ```

mod config;

pub use crate::config::{Config, ConfigParseError};
