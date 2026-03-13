# Design: FromStr for Config

## Status
Implemented

## Summary
Implement the `std::str::FromStr` trait for the `Config` type so that users can parse a configuration from a comma-separated key=value string (e.g., `"key1=val1,key2=val2"`). Parsing returns a descriptive error when the input format is invalid, telling the user exactly what went wrong and where.

## Motivation
Users need a simple, idiomatic way to construct a `Config` from a single string -- for example, when reading configuration from a CLI argument, an environment variable, or a configuration file field. Implementing `FromStr` is the standard Rust approach: it enables `"host=localhost,port=8080".parse::<Config>()` and integrates automatically with libraries like `clap` that call `FromStr` on argument values.

## Design

### Public API Changes

**New type: `ConfigParseError`**

```rust
/// Error returned when a config string cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigParseError {
    /// The overall input string is empty.
    EmptyInput,
    /// A segment (the part between commas) is missing the `=` delimiter.
    MissingEquals { segment: String },
    /// A segment has an empty key (e.g., `"=val"`).
    EmptyKey { segment: String },
    /// A segment has an empty value (e.g., `"key="`).
    EmptyValue { segment: String },
    /// A duplicate key was encountered.
    DuplicateKey { key: String },
}
```

`ConfigParseError` implements `std::fmt::Display` and `std::error::Error`.

**`FromStr` impl for `Config`**

```rust
impl FromStr for Config {
    type Err = ConfigParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> { ... }
}
```

**Existing `Config` type (new)**

```rust
/// A simple key-value configuration store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    entries: std::collections::HashMap<String, String>,
}
```

With accessor methods:
- `Config::get(&self, key: &str) -> Option<&str>`
- `Config::len(&self) -> usize`
- `Config::is_empty(&self) -> bool`
- `Config::keys(&self) -> impl Iterator<Item = &str>`

### Internal Design

1. Check for empty input; return `EmptyInput` if the trimmed string is empty.
2. Split the input on `,`.
3. For each segment:
   a. Trim whitespace.
   b. Split on the first `=` (`splitn(2, '=')`).
   c. Validate that both key and value are non-empty after trimming.
   d. Check for duplicate keys against the map built so far.
   e. Insert into a `HashMap<String, String>`.
4. Return `Config { entries }`.

Using `splitn(2, '=')` ensures that values containing `=` are handled correctly (e.g., `"url=https://example.com?a=1"` parses as key `url`, value `https://example.com?a=1`).

### Error Handling

Every validation failure maps to a specific `ConfigParseError` variant that carries the offending segment or key, so the caller can produce a user-friendly message. `Display` is implemented to produce messages like:

- `"config string is empty"`
- `"segment 'foobar' is missing '=' delimiter"`
- `"segment '=val' has an empty key"`
- `"segment 'key=' has an empty value"`
- `"duplicate key 'host'"`

## Alternatives Considered

**Alternative: serde-based deserialization.** We could implement `Deserialize` and use a custom deserializer for the `key=val,key=val` format. This was rejected because it adds a dependency on serde for a simple task, and `FromStr` is the idiomatic Rust trait for string parsing. Serde can always be added later as a non-breaking addition.

**Alternative: Return `HashMap` directly instead of a `Config` wrapper.** Rejected because a dedicated type gives us room to add methods, enforce invariants, and implement additional traits without breaking changes.

## Testing Plan

Unit tests in a `#[cfg(test)]` module covering:

1. **Happy path**: `"key1=val1,key2=val2"` parses into correct entries.
2. **Single entry**: `"key=val"` works.
3. **Values containing `=`**: `"url=https://a.com?x=1"` keeps the full value.
4. **Whitespace tolerance**: `" key1 = val1 , key2 = val2 "` trims correctly.
5. **Empty input**: `""` returns `EmptyInput`.
6. **Missing `=`**: `"foobar"` returns `MissingEquals`.
7. **Empty key**: `"=val"` returns `EmptyKey`.
8. **Empty value**: `"key="` returns `EmptyValue`.
9. **Duplicate key**: `"a=1,a=2"` returns `DuplicateKey`.
10. **Display impl**: Each error variant produces the expected human-readable message.

## Unresolved Questions

- Should whitespace inside keys/values be preserved (e.g., `"greeting=hello world"`)? Current design: yes, only leading/trailing whitespace per segment and per key/value is trimmed.
- Should we support an escape mechanism for commas or equals signs within values? Current design: no, keeping the initial implementation simple. Can be added later without breaking changes.
