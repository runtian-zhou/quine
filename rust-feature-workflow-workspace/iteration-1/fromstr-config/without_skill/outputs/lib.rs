pub mod error;

use std::collections::HashMap;
use std::str::FromStr;

use error::ConfigParseError;

/// A simple key-value configuration holder.
///
/// # Parsing
///
/// `Config` implements [`FromStr`] so it can be created from a comma-separated
/// list of `key=value` pairs:
///
/// ```
/// use std::str::FromStr;
/// let cfg = Config::from_str("host=localhost,port=8080").unwrap();
/// assert_eq!(cfg.get("host"), Some("localhost"));
/// assert_eq!(cfg.get("port"), Some("8080"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    entries: HashMap<String, String>,
}

impl Config {
    /// Returns the value associated with `key`, if present.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|v| v.as_str())
    }

    /// Returns the number of key-value pairs in the config.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the config contains no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns an iterator over all keys.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(|k| k.as_str())
    }
}

impl FromStr for Config {
    type Err = ConfigParseError;

    /// Parses a `Config` from a string of the form `key1=val1,key2=val2`.
    ///
    /// # Rules
    /// - Entries are separated by commas.
    /// - Each entry must contain exactly one `=` separating a non-empty key
    ///   from a non-empty value. (Values may contain additional `=` characters,
    ///   e.g. `token=abc==` is valid with key `token` and value `abc==`.)
    /// - Leading and trailing whitespace on keys and values is trimmed.
    /// - Duplicate keys are rejected.
    /// - An empty input string is rejected.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(ConfigParseError::EmptyInput);
        }

        let mut entries = HashMap::new();

        for segment in trimmed.split(',') {
            let segment = segment.trim();

            // Find the first '=' — everything before it is the key,
            // everything after (which may contain more '=' chars) is the value.
            let eq_pos = segment
                .find('=')
                .ok_or_else(|| ConfigParseError::MissingEquals {
                    segment: segment.to_string(),
                })?;

            let key = segment[..eq_pos].trim();
            let value = segment[eq_pos + 1..].trim();

            if key.is_empty() {
                return Err(ConfigParseError::EmptyKey {
                    segment: segment.to_string(),
                });
            }

            if value.is_empty() {
                return Err(ConfigParseError::EmptyValue {
                    key: key.to_string(),
                });
            }

            if entries.contains_key(key) {
                return Err(ConfigParseError::DuplicateKey {
                    key: key.to_string(),
                });
            }

            entries.insert(key.to_string(), value.to_string());
        }

        Ok(Config { entries })
    }
}
