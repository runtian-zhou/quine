use std::fmt;
use std::path::PathBuf;

pub mod telegram;

#[derive(Debug, Clone, Copy)]
struct BuiltInPlugin {
    name: &'static str,
    description: &'static str,
}

impl fmt::Display for BuiltInPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:<12} {}", self.name, self.description)
    }
}

const BUILT_IN_PLUGINS: &[BuiltInPlugin] = &[BuiltInPlugin {
    name: "telegram",
    description: "Relay Telegram messages to Quine and send the reply back.",
}];

pub fn list_plugins() {
    println!("{:<12} Description", "Name");
    println!("{:-<12} {:-<}", "", "");
    for plugin in BUILT_IN_PLUGINS {
        println!("{plugin}");
    }
}

pub fn spawn_autostart_plugins(socket_path: PathBuf) {
    tokio::spawn(async move {
        if let Err(error) = telegram::run_autostart(&socket_path).await {
            eprintln!("Telegram plugin autostart failed: {error}");
        }
    });
}
