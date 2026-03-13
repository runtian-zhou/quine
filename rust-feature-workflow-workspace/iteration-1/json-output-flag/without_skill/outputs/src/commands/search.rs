use serde::Serialize;
use std::fmt;

use crate::output::{self, OutputFormat};

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub path: String,
    pub score: f64,
}

impl fmt::Display for SearchResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}  {}", self.score, self.path)
    }
}

#[derive(Debug, Serialize)]
pub struct SearchOutput {
    pub query: String,
    pub total_results: usize,
    pub results: Vec<SearchResult>,
}

impl fmt::Display for SearchOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Search: \"{}\"", self.query)?;
        writeln!(f, "Found {} result(s):", self.total_results)?;
        for r in &self.results {
            writeln!(f, "  {r}")?;
        }
        Ok(())
    }
}

pub fn run(query: &str, limit: usize, format: &OutputFormat) -> anyhow::Result<()> {
    let results = perform_search(query, limit)?;
    let search_output = SearchOutput {
        query: query.to_string(),
        total_results: results.len(),
        results,
    };
    output::render(&search_output, format)
}

/// Walk the current directory and return files whose name contains the query
/// string, scored by how early the match appears in the filename.
fn perform_search(query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
    let mut results = Vec::new();
    walk_dir(std::path::Path::new("."), query, &mut results)?;
    // Sort best (highest) score first.
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    results.truncate(limit);
    Ok(results)
}

fn walk_dir(
    dir: &std::path::Path,
    query: &str,
    results: &mut Vec<SearchResult>,
) -> anyhow::Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    for entry in rd {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.contains(query) {
            let score = 1.0 / (1.0 + name.find(query).unwrap_or(0) as f64);
            results.push(SearchResult {
                path: path.to_string_lossy().into_owned(),
                score,
            });
        }
        if path.is_dir() {
            walk_dir(&path, query, results)?;
        }
    }
    Ok(())
}
