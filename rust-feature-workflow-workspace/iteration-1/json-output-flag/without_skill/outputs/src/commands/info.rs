use serde::Serialize;
use std::fmt;

use crate::output::{self, OutputFormat};

#[derive(Debug, Serialize)]
pub struct ItemInfo {
    pub name: String,
    pub exists: bool,
    pub is_file: bool,
    pub is_directory: bool,
    pub size_bytes: Option<u64>,
    pub permissions_octal: Option<String>,
}

impl fmt::Display for ItemInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Name:        {}", self.name)?;
        writeln!(f, "Exists:      {}", self.exists)?;
        if self.exists {
            writeln!(f, "Type:        {}", if self.is_directory { "directory" } else { "file" })?;
            if let Some(size) = self.size_bytes {
                writeln!(f, "Size:        {} bytes", size)?;
            }
            if let Some(ref perm) = self.permissions_octal {
                write!(f, "Permissions: {perm}")?;
            }
        }
        Ok(())
    }
}

pub fn run(name: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let path = std::path::Path::new(name);
    let info = if path.exists() {
        let meta = std::fs::metadata(path)?;
        #[cfg(unix)]
        let permissions_octal = {
            use std::os::unix::fs::PermissionsExt;
            Some(format!("{:o}", meta.permissions().mode()))
        };
        #[cfg(not(unix))]
        let permissions_octal = None;

        ItemInfo {
            name: name.to_string(),
            exists: true,
            is_file: meta.is_file(),
            is_directory: meta.is_dir(),
            size_bytes: Some(meta.len()),
            permissions_octal,
        }
    } else {
        ItemInfo {
            name: name.to_string(),
            exists: false,
            is_file: false,
            is_directory: false,
            size_bytes: None,
            permissions_octal: None,
        }
    };

    output::render(&info, format)
}
