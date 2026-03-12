#[cfg(test)]
mod tests {
    use std::str::FromStr;

    // In a real project these would be:
    //   use crate::{Config, error::ConfigParseError};
    // For illustration we use a path that assumes `tests.rs` is included from lib.rs
    // or lives in a `tests/` directory with `use your_crate::...`.
    use super::*;
    use crate::error::ConfigParseError;

    // -----------------------------------------------------------------------
    // Happy-path tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_single_entry() {
        let cfg = Config::from_str("host=localhost").unwrap();
        assert_eq!(cfg.len(), 1, "a single key=value pair produces exactly 1 entry");
        assert_eq!(
            cfg.get("host"),
            Some("localhost"),
            "the key 'host' must map to 'localhost'"
        );
    }

    #[test]
    fn parse_multiple_entries() {
        let cfg = Config::from_str("host=localhost,port=8080,debug=true").unwrap();
        assert_eq!(cfg.len(), 3, "three comma-separated pairs produce exactly 3 entries");
        assert_eq!(
            cfg.get("host"),
            Some("localhost"),
            "'host' must be 'localhost'"
        );
        assert_eq!(
            cfg.get("port"),
            Some("8080"),
            "'port' must be '8080'"
        );
        assert_eq!(
            cfg.get("debug"),
            Some("true"),
            "'debug' must be 'true'"
        );
    }

    #[test]
    fn whitespace_is_trimmed() {
        let cfg = Config::from_str("  host = localhost , port = 8080  ").unwrap();
        assert_eq!(cfg.len(), 2, "whitespace-padded input still yields exactly 2 entries");
        assert_eq!(
            cfg.get("host"),
            Some("localhost"),
            "leading/trailing spaces around key and value must be stripped"
        );
        assert_eq!(
            cfg.get("port"),
            Some("8080"),
            "leading/trailing spaces around key and value must be stripped"
        );
    }

    #[test]
    fn value_may_contain_equals_sign() {
        // Only the *first* '=' is the delimiter; the rest belong to the value.
        let cfg = Config::from_str("token=abc==,name=test").unwrap();
        assert_eq!(cfg.len(), 2, "two entries expected even when value contains '='");
        assert_eq!(
            cfg.get("token"),
            Some("abc=="),
            "the value for 'token' must be 'abc==' (extra '=' preserved)"
        );
        assert_eq!(
            cfg.get("name"),
            Some("test"),
            "'name' must be 'test'"
        );
    }

    #[test]
    fn get_missing_key_returns_none() {
        let cfg = Config::from_str("a=1").unwrap();
        assert_eq!(
            cfg.get("nonexistent"),
            None,
            "looking up a key that was never set must return None"
        );
    }

    // -----------------------------------------------------------------------
    // Error-path tests
    // -----------------------------------------------------------------------

    #[test]
    fn empty_string_is_rejected() {
        let err = Config::from_str("").unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::EmptyInput,
            "an empty string must produce EmptyInput, not any other variant"
        );
    }

    #[test]
    fn whitespace_only_string_is_rejected() {
        let err = Config::from_str("   ").unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::EmptyInput,
            "a whitespace-only string must produce EmptyInput"
        );
    }

    #[test]
    fn missing_equals_is_rejected() {
        let err = Config::from_str("hostlocalhost").unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::MissingEquals {
                segment: "hostlocalhost".to_string()
            },
            "a segment without '=' must produce MissingEquals with the offending segment"
        );
    }

    #[test]
    fn missing_equals_in_later_segment() {
        let err = Config::from_str("host=localhost,bad_segment").unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::MissingEquals {
                segment: "bad_segment".to_string()
            },
            "MissingEquals must report the exact segment that lacked '='"
        );
    }

    #[test]
    fn empty_key_is_rejected() {
        let err = Config::from_str("=value").unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::EmptyKey {
                segment: "=value".to_string()
            },
            "a segment whose key portion is empty must produce EmptyKey"
        );
    }

    #[test]
    fn empty_value_is_rejected() {
        let err = Config::from_str("key=").unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::EmptyValue {
                key: "key".to_string()
            },
            "a segment whose value portion is empty must produce EmptyValue with the key name"
        );
    }

    #[test]
    fn duplicate_key_is_rejected() {
        let err = Config::from_str("a=1,b=2,a=3").unwrap_err();
        assert_eq!(
            err,
            ConfigParseError::DuplicateKey {
                key: "a".to_string()
            },
            "repeating a key must produce DuplicateKey with that key"
        );
    }

    // -----------------------------------------------------------------------
    // Error Display tests (verify messages are human-friendly)
    // -----------------------------------------------------------------------

    #[test]
    fn error_display_empty_input() {
        let msg = ConfigParseError::EmptyInput.to_string();
        assert_eq!(
            msg,
            "config string is empty; expected format: 'key1=val1,key2=val2'",
            "EmptyInput display must include the expected format hint"
        );
    }

    #[test]
    fn error_display_missing_equals() {
        let msg = ConfigParseError::MissingEquals {
            segment: "oops".to_string(),
        }
        .to_string();
        assert_eq!(
            msg,
            "segment 'oops' is missing an '=' delimiter; each entry must be 'key=value'",
            "MissingEquals display must name the offending segment and explain the format"
        );
    }

    #[test]
    fn error_display_empty_key() {
        let msg = ConfigParseError::EmptyKey {
            segment: "=val".to_string(),
        }
        .to_string();
        assert_eq!(
            msg,
            "found an empty key in segment '=val'; keys must be non-empty",
            "EmptyKey display must show the segment"
        );
    }

    #[test]
    fn error_display_empty_value() {
        let msg = ConfigParseError::EmptyValue {
            key: "port".to_string(),
        }
        .to_string();
        assert_eq!(
            msg,
            "key 'port' has an empty value; every key must map to a non-empty value",
            "EmptyValue display must name the key"
        );
    }

    #[test]
    fn error_display_duplicate_key() {
        let msg = ConfigParseError::DuplicateKey {
            key: "host".to_string(),
        }
        .to_string();
        assert_eq!(
            msg,
            "key 'host' appears more than once",
            "DuplicateKey display must name the duplicated key"
        );
    }

    // -----------------------------------------------------------------------
    // Config utility method tests
    // -----------------------------------------------------------------------

    #[test]
    fn is_empty_returns_false_for_populated_config() {
        let cfg = Config::from_str("a=1").unwrap();
        assert_eq!(cfg.is_empty(), false, "a config with entries is not empty");
    }

    #[test]
    fn keys_returns_all_keys() {
        let cfg = Config::from_str("x=1,y=2,z=3").unwrap();
        let mut keys: Vec<&str> = cfg.keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["x", "y", "z"],
            "keys() must yield exactly the keys that were parsed, sorted: [x, y, z]"
        );
    }

    // -----------------------------------------------------------------------
    // std::error::Error trait conformance
    // -----------------------------------------------------------------------

    #[test]
    fn error_implements_std_error() {
        // This is a compile-time check: if ConfigParseError doesn't implement
        // std::error::Error the function reference won't type-check.
        fn assert_std_error<E: std::error::Error>() {}
        assert_std_error::<ConfigParseError>();
    }
}
