//! Output formatting module.
//!
//! Provides [`OutputFormat`], [`OutputWriter`], and the [`CommandResult`] trait
//! so that every CLI command can emit either human-readable text or structured
//! JSON through a single code path.

use serde::Serialize;
use std::fmt;
use std::io::{self, Write};

// ---------------------------------------------------------------------------
// OutputFormat
// ---------------------------------------------------------------------------

/// The format the CLI should use when printing results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Human-readable text (the default).
    Human,
    /// Structured JSON, suitable for piping to `jq`.
    Json,
}

// ---------------------------------------------------------------------------
// CommandResult trait
// ---------------------------------------------------------------------------

/// Marker trait for command results.
///
/// Every command handler returns a type that implements both [`Serialize`]
/// (for JSON output) and [`fmt::Display`] (for human-readable output).
pub trait CommandResult: Serialize + fmt::Display {}

// Blanket implementation: any type that is Serialize + Display is a CommandResult.
impl<T: Serialize + fmt::Display> CommandResult for T {}

// ---------------------------------------------------------------------------
// JSON envelopes
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct SuccessEnvelope<'a, T: Serialize> {
    status: &'a str,
    data: &'a T,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
}

// ---------------------------------------------------------------------------
// OutputWriter
// ---------------------------------------------------------------------------

/// Handles writing command results to stdout in the configured format.
pub struct OutputWriter {
    format: OutputFormat,
}

impl OutputWriter {
    /// Create a new writer for the given format.
    pub fn new(format: OutputFormat) -> Self {
        Self { format }
    }

    /// Write a successful command result to stdout.
    ///
    /// In [`OutputFormat::Human`] mode, this calls the value's [`Display`] impl.
    /// In [`OutputFormat::Json`] mode, this serializes the value inside a
    /// `{"status": "ok", "data": ...}` envelope.
    pub fn write_result<T: CommandResult>(&self, result: &T) -> io::Result<()> {
        let stdout = io::stdout();
        let mut handle = stdout.lock();

        match self.format {
            OutputFormat::Human => {
                writeln!(handle, "{}", result)
            }
            OutputFormat::Json => {
                let envelope = SuccessEnvelope {
                    status: "ok",
                    data: result,
                };
                let json = serde_json::to_string_pretty(&envelope)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                writeln!(handle, "{}", json)
            }
        }
    }

    /// Write an error to stdout (JSON mode) or stderr (human mode).
    ///
    /// Returns the suggested process exit code (always 1).
    pub fn write_error(&self, code: &str, message: &str) -> io::Result<i32> {
        match self.format {
            OutputFormat::Human => {
                eprintln!("Error: {}", message);
            }
            OutputFormat::Json => {
                let envelope = ErrorEnvelope {
                    error: ErrorBody { code, message },
                };
                let json = serde_json::to_string_pretty(&envelope)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                let stdout = io::stdout();
                let mut handle = stdout.lock();
                writeln!(handle, "{}", json)?;
            }
        }
        Ok(1)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::fmt;

    /// A sample result type used in tests.
    #[derive(Serialize)]
    struct SampleResult {
        name: String,
        count: u64,
    }

    impl fmt::Display for SampleResult {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}: {}", self.name, self.count)
        }
    }

    #[test]
    fn test_human_output_uses_display() {
        // In human mode, write_result should use the Display impl.
        // We cannot easily capture stdout in a unit test, so we verify
        // the format selection logic and Display output independently.
        let result = SampleResult {
            name: "widgets".to_string(),
            count: 42,
        };
        let display_output = format!("{}", result);
        assert_eq!(display_output, "widgets: 42", "Display impl should format as 'name: count'");
    }

    #[test]
    fn test_json_serialization_produces_correct_envelope() {
        let result = SampleResult {
            name: "widgets".to_string(),
            count: 42,
        };
        let envelope = SuccessEnvelope {
            status: "ok",
            data: &result,
        };
        let json: serde_json::Value = serde_json::to_value(&envelope).unwrap();

        assert_eq!(json["status"], "ok", "Envelope status must be 'ok' for success");
        assert_eq!(json["data"]["name"], "widgets", "Data must contain the serialized name field");
        assert_eq!(json["data"]["count"], 42, "Data must contain the exact count value");
    }

    #[test]
    fn test_error_envelope_structure() {
        let envelope = ErrorEnvelope {
            error: ErrorBody {
                code: "not_found",
                message: "File 'foo.txt' not found",
            },
        };
        let json: serde_json::Value = serde_json::to_value(&envelope).unwrap();

        assert_eq!(
            json["error"]["code"], "not_found",
            "Error code must be exactly 'not_found'"
        );
        assert_eq!(
            json["error"]["message"], "File 'foo.txt' not found",
            "Error message must match the input exactly"
        );
    }

    #[test]
    fn test_output_format_equality() {
        assert_eq!(OutputFormat::Human, OutputFormat::Human, "Human == Human");
        assert_eq!(OutputFormat::Json, OutputFormat::Json, "Json == Json");
        assert_ne!(OutputFormat::Human, OutputFormat::Json, "Human != Json");
    }

    #[test]
    fn test_json_output_with_special_characters() {
        let result = SampleResult {
            name: "line1\nline2\ttab \"quoted\"".to_string(),
            count: 0,
        };
        let envelope = SuccessEnvelope {
            status: "ok",
            data: &result,
        };
        let json_str = serde_json::to_string(&envelope).unwrap();
        // Verify it round-trips correctly
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(
            parsed["data"]["name"], "line1\nline2\ttab \"quoted\"",
            "Special characters must survive JSON round-trip exactly"
        );
        assert_eq!(
            parsed["data"]["count"], 0,
            "Zero count must serialize as exactly 0"
        );
    }

    #[test]
    fn test_empty_string_result() {
        let result = SampleResult {
            name: "".to_string(),
            count: 0,
        };
        let envelope = SuccessEnvelope {
            status: "ok",
            data: &result,
        };
        let json: serde_json::Value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["data"]["name"], "", "Empty string must serialize as empty string, not null");
        assert_eq!(json["data"]["count"], 0, "Count must be exactly 0");
    }
}
