use std::fmt;

/// Enumerates all the ways parsing a `Config` from a string can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigParseError {
    /// The input string was empty.
    EmptyInput,
    /// A key-value segment (between commas) did not contain an '=' delimiter.
    MissingEquals {
        /// The segment that was malformed.
        segment: String,
    },
    /// A key was empty (e.g. "=val" or "key1=val1,=val2").
    EmptyKey {
        /// The segment that contained the empty key.
        segment: String,
    },
    /// A value was empty (e.g. "key=" or "key1=val1,key2=").
    EmptyValue {
        /// The key whose value was missing.
        key: String,
    },
    /// A key appeared more than once.
    DuplicateKey {
        /// The duplicated key.
        key: String,
    },
}

impl fmt::Display for ConfigParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigParseError::EmptyInput => {
                write!(
                    f,
                    "config string is empty; expected format: 'key1=val1,key2=val2'"
                )
            }
            ConfigParseError::MissingEquals { segment } => {
                write!(
                    f,
                    "segment '{}' is missing an '=' delimiter; each entry must be 'key=value'",
                    segment
                )
            }
            ConfigParseError::EmptyKey { segment } => {
                write!(
                    f,
                    "found an empty key in segment '{}'; keys must be non-empty",
                    segment
                )
            }
            ConfigParseError::EmptyValue { key } => {
                write!(
                    f,
                    "key '{}' has an empty value; every key must map to a non-empty value",
                    key
                )
            }
            ConfigParseError::DuplicateKey { key } => {
                write!(f, "key '{}' appears more than once", key)
            }
        }
    }
}

impl std::error::Error for ConfigParseError {}
