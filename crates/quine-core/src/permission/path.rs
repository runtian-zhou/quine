use std::path::{Component, Path, PathBuf};

use super::context::PermissionContext;
use super::request::PermissionScope;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorizedPath {
    pub resolved_path: PathBuf,
}

fn normalize_path(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
        }
    }
    Some(normalized)
}

fn canonical_or_normalized(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Some(canonical);
    }

    let normalized = normalize_path(path)?;
    for ancestor in normalized.ancestors() {
        if !ancestor.exists() {
            continue;
        }

        let canonical_ancestor = std::fs::canonicalize(ancestor).ok()?;
        if ancestor == normalized {
            return Some(canonical_ancestor);
        }

        let suffix = normalized.strip_prefix(ancestor).ok()?;
        return Some(canonical_ancestor.join(suffix));
    }

    Some(normalized)
}

fn in_allowed_roots(path: &Path, allowed_roots: &[PathBuf]) -> bool {
    allowed_roots.iter().any(|root| {
        canonical_or_normalized(root)
            .map(|normalized_root| path.starts_with(&normalized_root))
            .unwrap_or(false)
    })
}

pub(crate) fn authorize_path(
    context: &PermissionContext,
    scope: PermissionScope,
    path: &Path,
) -> Result<AuthorizedPath, String> {
    let allowed_roots = context.approved_roots();
    let resolved_path = canonical_or_normalized(path).ok_or_else(|| {
        format!(
            "failed to resolve path for {:?} access: {}",
            scope,
            path.display()
        )
    })?;

    if in_allowed_roots(&resolved_path, &allowed_roots) {
        return Ok(AuthorizedPath { resolved_path });
    }

    Err(format!(
        "{:?} access denied outside approved roots: {}",
        scope,
        resolved_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::context::PermissionContext;
    use crate::permission::request::PermissionScope;
    use crate::permission::types::PermissionPromptBehavior;
    use tempfile::TempDir;

    #[test]
    fn workspace_root_allows_resolved_in_bounds_paths() {
        let workspace = TempDir::new().unwrap();
        let target = workspace.path().join("src").join("..").join("Cargo.toml");
        let context = PermissionContext::new(
            workspace.path().to_path_buf(),
            false,
            PermissionPromptBehavior::Interactive,
        );

        let authorized = authorize_path(&context, PermissionScope::Read, &target).unwrap();
        assert!(authorized
            .resolved_path
            .starts_with(std::fs::canonicalize(workspace.path()).unwrap()));
    }

    #[test]
    fn additional_root_allows_resolved_in_bounds_paths() {
        let workspace = TempDir::new().unwrap();
        let additional = TempDir::new().unwrap();
        let context = {
            let mut context = PermissionContext::new(
                workspace.path().to_path_buf(),
                false,
                PermissionPromptBehavior::Interactive,
            );
            context.add_allowed_root(additional.path().to_path_buf());
            context
        };

        let authorized = authorize_path(
            &context,
            PermissionScope::Read,
            &additional.path().join("notes").join("allowed.txt"),
        )
        .unwrap();
        assert!(authorized
            .resolved_path
            .starts_with(std::fs::canonicalize(additional.path()).unwrap()));
    }

    #[test]
    fn outside_all_roots_is_denied() {
        let workspace = TempDir::new().unwrap();
        let additional = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let context = {
            let mut context = PermissionContext::new(
                workspace.path().to_path_buf(),
                false,
                PermissionPromptBehavior::Interactive,
            );
            context.add_allowed_root(additional.path().to_path_buf());
            context
        };

        let error = authorize_path(
            &context,
            PermissionScope::Write,
            &outside.path().join("forbidden.txt"),
        )
        .unwrap_err();
        assert!(error.contains("outside approved roots"));
    }

    #[test]
    fn traversal_is_evaluated_on_final_resolved_target() {
        let workspace = TempDir::new().unwrap();
        let inside = workspace
            .path()
            .join("fixtures")
            .join("..")
            .join("allowed.txt");
        let context = PermissionContext::new(
            workspace.path().to_path_buf(),
            false,
            PermissionPromptBehavior::Interactive,
        );
        assert!(authorize_path(&context, PermissionScope::Read, &inside).is_ok());

        let outside_parent = workspace.path().parent().unwrap().join("outside.txt");
        let outside_via_traversal = workspace
            .path()
            .join("fixtures")
            .join("..")
            .join("..")
            .join(
                outside_parent
                    .file_name()
                    .expect("outside file name should exist"),
            );
        let error =
            authorize_path(&context, PermissionScope::Write, &outside_via_traversal).unwrap_err();
        assert!(error.contains("outside approved roots"));
    }

    #[test]
    fn symlink_escape_is_denied_if_supported() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("forbidden.txt");
        std::fs::write(&outside_file, "forbidden").unwrap();
        let symlink_path = workspace.path().join("link-out");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_file, &symlink_path).unwrap();
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_file(&outside_file, &symlink_path).is_err() {
                return;
            }
        }

        let context = PermissionContext::new(
            workspace.path().to_path_buf(),
            false,
            PermissionPromptBehavior::Interactive,
        );
        let error = authorize_path(&context, PermissionScope::Read, &symlink_path).unwrap_err();
        assert!(error.contains("outside approved roots"));
    }
}
