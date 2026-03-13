use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// A simple key-value configuration store.
///
/// # Examples
///
/// ```
/// use config::Config;
///
/// let cfg: Config = "host=localhost,port=8080".parse().unwrap();
/// assert_eq!(cfg.get("host"), Some("localhost"));
/// assert_eq!(cfg.get("port"), Some("8080"));
/// assert_eq!(cfg.len(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    entries: HashMap<String, String>,
}

impl Config {
    /// Returns the value associated with the given key, or `None` if the key
    /// is not present.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|v| v.as_str())
    }

    /// Returns the number of key-value pairs in this configuration.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if this configuration contains no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns an iterator over the keys in this configuration.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(|k| k.as_str())
    }
}

/// Error returned when a config string cannot be parsed.
///
/// Each variant carries enough context for the caller to produce a
/// user-friendly diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigParseError {
    /// The input string is empty (or only whitespace).
    EmptyInput,
    /// A segment between commas is missing the `=` delimiter.
    MissingEquals { segment: String },
    /// A segment has an empty key (e.g., `"=val"`).
    EmptyKey { segment: String },
    /// A segment has an empty value (e.g., `"key="`).
    EmptyValue { segment: String },
    /// A key appeared more than once.
    DuplicateKey { key: String },
}

impl fmt::Display for ConfigParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigParseError::EmptyInput => write!(f, "config string is empty"),
            ConfigParseError::MissingEquals { segment } => {
                write!(f, "segment '{}' is missing '=' delimiter", segment)
            }
            ConfigParseError::EmptyKey { segment } => {
                write!(f, "segment '{}' has an empty key", segment)
            }
            ConfigParseError::EmptyValue { segment } => {
                write!(f, "segment '{}' has an empty value", segment)
            }
            ConfigParseError::DuplicateKey { key } => {
                write!(f, "duplicate key '{}'", key)
            }
        }
    }
}

impl std::error::Error for ConfigParseError {}

impl FromStr for Config {
    type Err = ConfigParseError;

    /// Parses a config string in the format `"key1=val1,key2=val2"`.
    ///
    /// - Leading and trailing whitespace around each key and value is trimmed.
    /// - Values may contain `=` characters (only the first `=` is the delimiter).
    /// - Duplicate keys are rejected.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigParseError`] describing exactly what went wrong.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(ConfigParseError::EmptyInput);
        }

        let mut entries = HashMap::new();

        for segment in trimmed.split(',') {
            let segment = segment.trim();

            // Split on the first '=' only, so values can contain '='.
            let mut parts = segment.splitn(2, '=');
            let key = parts.next().unwrap_or("").trim();
            let value = match parts.next() {
                Some(v) => v.trim(),
                None => {
                    return Err(ConfigParseError::MissingEquals {
                        segment: segment.to_string(),
                    });
                }
            };

            if key.is_empty() {
                return Err(ConfigParseError::EmptyKey {
                    segment: segment.to_string(),
                });
            }

            if value.is_empty() {
                return Err(ConfigParseError::EmptyValue {
                    segment: segment.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_multiple_entries() {
        let cfg: Config = "key1=val1,key2=val2".parse().unwrap();
        assert_eq!(cfg.len(), 2, "expected exactly 2 entries");
        assert_eq!(
            cfg.get("key1"),
            Some("val1"),
            "key1 should map to val1"
        );
        assert_eq!(
            cfg.get("key2"),
            Some("val2"),
            "key2 should map to val2"
        );
    }

    #[test]
    fn parse_single_entry() {
        let cfg: Config = "host=localhost".parse().unwrap();
        assert_eq!(cfg.len(), 1, "expected exactly 1 entry");
        assert_eq!(
            cfg.get("host"),
            Some("localhost"),
            "host should map to localhost"
        );
    }

    #[test]
    fn value_containing_equals() {
        // The value itself contains '=', which should be preserved.
        let cfg: Config = "url=https://example.com?a=1&b=2".parse().unwrap();
        assert_eq!(cfg.len(), 1, "expected exactly 1 entry");
        assert_eq!(
            cfg.get("url"),
            Some("https://example.com?a=1&b=2"),
            "the full URL with query params should be the value"
        );
    }

    #[test]
    fn whitespace_is_trimmed() {
        let cfg: Config = "  key1 = val1 , key2 = val2  ".parse().unwrap();
        assert_eq!(cfg.len(), 2, "expected exactly 2 entries after trimming");
        assert_eq!(
            cfg.get("key1"),
            Some("val1"),
            "leading/trailing whitespace around key1 and val1 should be trimmed"
        );
        assert_eq!(
            cfg.get("key2"),
            Some("val2"),
            "leading/trailing whitespace around key2 and val2 should be trimmed"
        );
    }

    #[test]
    fn empty_input_returns_error() {
        let err = "".parse::<Config>().unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::EmptyInput,
            "empty string should produce EmptyInput error"
        );
    }

    #[test]
    fn whitespace_only_input_returns_error() {
        let err = "   ".parse::<Config>().unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::EmptyInput,
            "whitespace-only string should produce EmptyInput error"
        );
    }

    #[test]
    fn missing_equals_returns_error() {
        let err = "foobar".parse::<Config>().unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::MissingEquals {
                segment: "foobar".to_string()
            },
            "segment without '=' should produce MissingEquals error"
        );
    }

    #[test]
    fn empty_key_returns_error() {
        let err = "=val".parse::<Config>().unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::EmptyKey {
                segment: "=val".to_string()
            },
            "segment with empty key should produce EmptyKey error"
        );
    }

    #[test]
    fn empty_value_returns_error() {
        let err = "key=".parse::<Config>().unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::EmptyValue {
                segment: "key=".to_string()
            },
            "segment with empty value should produce EmptyValue error"
        );
    }

    #[test]
    fn duplicate_key_returns_error() {
        let err = "a=1,a=2".parse::<Config>().unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::DuplicateKey {
                key: "a".to_string()
            },
            "repeated key should produce DuplicateKey error"
        );
    }

    #[test]
    fn display_impl_empty_input() {
        let msg = ConfigParseError::EmptyInput.to_string();
        assert_eq!(
            msg, "config string is empty",
            "EmptyInput display should be 'config string is empty'"
        );
    }

    #[test]
    fn display_impl_missing_equals() {
        let msg = ConfigParseError::MissingEquals {
            segment: "foobar".to_string(),
        }
        .to_string();
        assert_eq!(
            msg, "segment 'foobar' is missing '=' delimiter",
            "MissingEquals display should mention the segment and missing delimiter"
        );
    }

    #[test]
    fn display_impl_empty_key() {
        let msg = ConfigParseError::EmptyKey {
            segment: "=val".to_string(),
        }
        .to_string();
        assert_eq!(
            msg, "segment '=val' has an empty key",
            "EmptyKey display should mention the segment"
        );
    }

    #[test]
    fn display_impl_empty_value() {
        let msg = ConfigParseError::EmptyValue {
            segment: "key=".to_string(),
        }
        .to_string();
        assert_eq!(
            msg, "segment 'key=' has an empty value",
            "EmptyValue display should mention the segment"
        );
    }

    #[test]
    fn display_impl_duplicate_key() {
        let msg = ConfigParseError::DuplicateKey {
            key: "host".to_string(),
        }
        .to_string();
        assert_eq!(
            msg, "duplicate key 'host'",
            "DuplicateKey display should mention the key"
        );
    }

    #[test]
    fn is_empty_returns_false_for_populated_config() {
        let cfg: Config = "a=1".parse().unwrap();
        assert_eq!(
            cfg.is_empty(),
            false,
            "a config with entries should not be empty"
        );
    }

    #[test]
    fn get_missing_key_returns_none() {
        let cfg: Config = "a=1".parse().unwrap();
        assert_eq!(
            cfg.get("nonexistent"),
            None,
            "looking up a key that was never set should return None"
        );
    }

    #[test]
    fn error_implements_std_error() {
        // Verify that ConfigParseError can be used as a dyn Error.
        let err: Box<dyn std::error::Error> = Box::new(ConfigParseError::EmptyInput);
        assert_eq!(
            err.to_string(),
            "config string is empty",
            "ConfigParseError should work as a trait object via std::error::Error"
        );
    }
}
