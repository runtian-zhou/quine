use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub provider: String,
    pub model: String,
    pub api_key: Option<String>,
    pub working_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl Config {
    pub fn new(provider: &str, model: &str) -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let log_dir = working_dir.join(".quine").join("logs");
        Self {
            provider: provider.to_string(),
            model: model.to_string(),
            api_key: None,
            working_dir,
            log_dir,
        }
    }
}
