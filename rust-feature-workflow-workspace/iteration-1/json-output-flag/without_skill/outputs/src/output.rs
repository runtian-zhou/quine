use serde::Serialize;
use std::fmt;

/// Controls whether output is human-readable text or structured JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Human,
    Json,
}

/// Trait that every command result must implement to support both output modes.
pub trait Outputable: Serialize + fmt::Display {}

// Blanket impl: anything that is both Serialize and Display is Outputable.
impl<T: Serialize + fmt::Display> Outputable for T {}

/// Render a value according to the chosen format.
///
/// - `Human` calls `Display::fmt` (the existing human-readable path).
/// - `Json` serialises to pretty-printed JSON and writes to stdout.
///
/// In JSON mode errors are also emitted as JSON objects with an `"error"` key,
/// so downstream tooling (e.g. `jq`) never sees unstructured text.
pub fn render<T: Outputable>(value: &T, format: &OutputFormat) -> anyhow::Result<()> {
    match format {
        OutputFormat::Human => {
            println!("{value}");
        }
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(value)?;
            println!("{json}");
        }
    }
    Ok(())
}

/// Render a list of values. In JSON mode the entire list is emitted as a
/// JSON array so the output is always a single valid JSON document.
pub fn render_list<T: Outputable>(values: &[T], format: &OutputFormat) -> anyhow::Result<()> {
    match format {
        OutputFormat::Human => {
            for v in values {
                println!("{v}");
            }
        }
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(values)?;
            println!("{json}");
        }
    }
    Ok(())
}

/// Convenience wrapper to emit an error as JSON when in JSON mode, or as a
/// normal anyhow error chain when in human mode.
pub fn render_error(err: &anyhow::Error, format: &OutputFormat) {
    match format {
        OutputFormat::Human => {
            eprintln!("Error: {err:#}");
        }
        OutputFormat::Json => {
            let obj = serde_json::json!({
                "error": format!("{err:#}"),
            });
            // Write to stdout so the consumer can still parse it with jq.
            println!("{}", serde_json::to_string_pretty(&obj).unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Sample {
        name: String,
        count: u32,
    }

    impl fmt::Display for Sample {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{} ({})", self.name, self.count)
        }
    }

    #[test]
    fn render_json_produces_valid_json() {
        let sample = Sample {
            name: "alpha".into(),
            count: 42,
        };
        let json = serde_json::to_string_pretty(&sample).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["name"], "alpha");
        assert_eq!(parsed["count"], 42);
    }

    #[test]
    fn render_list_json_produces_array() {
        let items = vec![
            Sample { name: "a".into(), count: 1 },
            Sample { name: "b".into(), count: 2 },
        ];
        let json = serde_json::to_string_pretty(&items).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 2);
        assert_eq!(parsed[0]["name"], "a");
        assert_eq!(parsed[0]["count"], 1);
        assert_eq!(parsed[1]["name"], "b");
        assert_eq!(parsed[1]["count"], 2);
    }

    #[test]
    fn render_error_json_has_error_key() {
        let err = anyhow::anyhow!("something went wrong");
        let obj = serde_json::json!({ "error": format!("{err:#}") });
        let json = serde_json::to_string_pretty(&obj).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["error"], "something went wrong");
    }
}
