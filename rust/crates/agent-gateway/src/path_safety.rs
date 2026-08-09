//! Path safety — enforces that every path used by the agent stays inside the
//! user's workspace root.
//!
//! ## Threat Model
//!
//! A malicious or buggy agent might try to access paths outside the workspace
//! using path traversal attacks (e.g. `../../../etc/passwd`). We must catch
//! and reject every such attempt.
//!
//! ## The Invariant
//!
//! For any path `p` used by the agent (file reads, writes, bash `cd`, git, etc.):
//!
//! ```text
//! normalize(p).starts_with(normalize(workspace_root))
//! ```
//!
//! We canonicalize both sides using `std::fs::canonicalize` so that symlinks,
//! `..`, `.`, and redundant slashes cannot bypass the check.

use crate::error::{GatewayError, Result};
use std::path::{Path, PathBuf};

/// Validates that a path stays inside the workspace root.
/// This is the ONLY mechanism that enforces user isolation.
#[derive(Debug, Clone)]
pub struct PathValidator {
    workspace_root: PathBuf,
    // Pre-canonicalized root for fast comparison
    canonical_root: PathBuf,
}

/// Strip URL-encoded path traversal sequences from a path string. Used as a
/// fallback when canonicalize fails to prevent bypass via `%2e%2e` tricks.
fn strip_url_encoded_traversal(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    // Strip %2e%2e (case-insensitive) iteratively to handle nested encoding
    let mut result = s.to_lowercase();
    loop {
        let next = result.replace("%2e%2e", "");
        if next == result {
            break;
        }
        result = next;
    }
    // Strip %2f (encoded slash) -> keep as /
    result = result.replace("%2f", "/");
    // Strip lone %2e -> .
    result = result.replace("%2e", ".");
    PathBuf::from(result)
}

/// Resolve `..` and `.` path components purely via string manipulation,
/// without filesystem access. Used as a fallback when canonicalize fails.
/// Always strips the `/private` prefix to handle macOS aliasing between
/// `/var/folders` and `/private/var/folders`.
fn normalize_dotdot(path: &Path) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut has_leading_slash = false;

    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(s) => {
                parts.push(s.to_str().unwrap_or(""));
            }
            std::path::Component::CurDir | std::path::Component::Prefix(_) => {}
            std::path::Component::RootDir => {
                has_leading_slash = true;
            }
        }
    }
    let mut normalized = parts.join("/");
    // Strip /private prefix to handle macOS aliasing
    normalized = normalized
        .strip_prefix("/private")
        .unwrap_or(&normalized)
        .to_string();
    if has_leading_slash && !normalized.is_empty() {
        format!("/{normalized}")
    } else if normalized.is_empty() {
        "/".to_string()
    } else {
        normalized
    }
}

impl PathValidator {
    /// Create a new validator for the given workspace root.
    /// The root MUST be an absolute path.
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        let workspace_root = workspace_root.into();
        if !workspace_root.is_absolute() {
            return Err(GatewayError::Internal(format!(
                "workspace_root must be absolute: {}",
                workspace_root.display()
            )));
        }

        // Ensure the directory exists
        std::fs::create_dir_all(&workspace_root).map_err(GatewayError::Io)?;

        // Canonicalize the root to resolve symlinks, .., etc.
        let canonical_root = std::fs::canonicalize(&workspace_root).map_err(GatewayError::Io)?;

        Ok(Self {
            workspace_root,
            canonical_root,
        })
    }

    /// Validate that `path` is inside the workspace root.
    ///
    /// - Resolves `..`, `.`, symlinks via `canonicalize`
    /// - Returns the canonicalized path on success
    ///
    /// # Security
    ///
    /// This is the **single choke point** for all path validation.
    /// Every file operation in the agent MUST go through here.
    pub fn validate(&self, path: impl AsRef<Path>) -> Result<PathBuf> {
        self.validate_impl(path.as_ref())
    }

    /// Like `validate` but returns None instead of an error.
    #[must_use]
    pub fn validate_opt(&self, path: impl AsRef<Path>) -> Option<PathBuf> {
        self.validate_impl(path.as_ref()).ok()
    }

    fn validate_impl(&self, path: &Path) -> Result<PathBuf> {
        if path.as_os_str().is_empty() {
            return Err(GatewayError::Internal("empty path".to_string()));
        }

        // Make absolute if relative (resolve against workspace root)
        let abs_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        };

        // Canonicalize to resolve .., ., symlinks
        let canonical = match std::fs::canonicalize(&abs_path) {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Non-existent path: strip embedded URL-encoded traversal sequences
                // (e.g. `..%2F`) from the path string so they can't be used to
                // bypass the containment check via string-prefix tricks.
                let stripped = strip_url_encoded_traversal(&abs_path);
                let norm_abs = normalize_dotdot(&stripped);
                let norm_root = normalize_dotdot(&self.canonical_root);
                // Normalize to lowercase for case-insensitive comparison (macOS is case-insensitive).
                // Also strip /private prefix to handle macOS /var/folders aliasing.
                let norm_abs_lower = norm_abs
                    .strip_prefix('/')
                    .unwrap_or(&norm_abs)
                    .trim_start_matches("private/")
                    .to_lowercase();
                let norm_root_lower = norm_root
                    .strip_prefix('/')
                    .unwrap_or(&norm_root)
                    .trim_start_matches("private/")
                    .to_lowercase();
                if !norm_abs_lower.starts_with(&norm_root_lower) {
                    return Err(GatewayError::PathEscape {
                        requested: abs_path.display().to_string(),
                        root: self.canonical_root.display().to_string(),
                    });
                }
                // The path is confirmed inside root — walk up to find an existing
                // ancestor and validate its canonical form (handles symlinks).
                let mut current = abs_path.as_path();
                loop {
                    match std::fs::canonicalize(current) {
                        Ok(canonical_ancestor) => {
                            if !canonical_ancestor.starts_with(&self.canonical_root) {
                                return Err(GatewayError::PathEscape {
                                    requested: abs_path.display().to_string(),
                                    root: self.canonical_root.display().to_string(),
                                });
                            }
                            return Ok(abs_path);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            let parent = current.parent().unwrap_or(current);
                            if parent == current {
                                return Err(GatewayError::Internal(format!(
                                    "path has no existing ancestor inside root: {}",
                                    abs_path.display()
                                )));
                            }
                            current = parent;
                        }
                        Err(e) => return Err(GatewayError::Io(e)),
                    }
                }
            }
            Err(e) => return Err(GatewayError::Io(e)),
        };

        // THE critical check: canonical path must start with canonical root
        if !canonical.starts_with(&self.canonical_root) {
            tracing::warn!(
                "path escape attempt blocked: {} (root: {})",
                canonical.display(),
                self.canonical_root.display()
            );
            return Err(GatewayError::PathEscape {
                requested: canonical.display().to_string(),
                root: self.canonical_root.display().to_string(),
            });
        }

        Ok(canonical)
    }

    /// Get the workspace root.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Get the canonical workspace root.
    #[must_use]
    pub fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    /// Check if a path is inside the workspace (no error on success).
    #[must_use]
    pub fn contains(&self, path: impl AsRef<Path>) -> bool {
        self.validate_opt(path).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent-gateway-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn validates_normal_paths() {
        let root = temp_workspace();
        let validator = PathValidator::new(&root).unwrap();

        assert!(validator.validate(root.join("file.txt")).is_ok());
        assert!(validator
            .validate(root.join("subdir/nested/file.txt"))
            .is_ok());
        assert!(validator.contains(root.join("file.txt")));
    }

    #[test]
    fn blocks_traversal_outside_root() {
        let root = temp_workspace();
        let validator = PathValidator::new(&root).unwrap();

        // Path traversal attempts
        let traversal_attempts = [
            format!("{}/../../../etc/passwd", root.display()),
            format!("{}/..%2F..%2F..%2Fetc/passwd", root.display()),
            format!("{}/foo/../../etc/passwd", root.display()),
        ];

        for attempt in &traversal_attempts {
            let result = validator.validate(attempt);
            assert!(
                matches!(result, Err(GatewayError::PathEscape { .. })),
                "Should block: {attempt}",
            );
        }
    }

    #[test]
    fn blocks_absolute_paths_outside_root() {
        let root = temp_workspace();
        let validator = PathValidator::new(&root).unwrap();

        // Absolute paths outside workspace must be blocked
        let result = validator.validate("/etc/passwd");
        assert!(matches!(result, Err(GatewayError::PathEscape { .. })));

        let result = validator.validate("/tmp/evil");
        assert!(matches!(result, Err(GatewayError::PathEscape { .. })));
    }

    #[test]
    fn allows_relative_paths_inside_root() {
        let root = temp_workspace();
        let validator = PathValidator::new(&root).unwrap();

        // Relative paths should be resolved inside workspace
        let result = validator.validate("subdir/file.txt");
        assert!(result.is_ok());
    }

    #[test]
    fn allows_new_file_in_valid_subdir() {
        let root = temp_workspace();
        let validator = PathValidator::new(&root).unwrap();

        // Non-existent file in existing subdir should be allowed
        let new_file = root.join("new-dir").join("new-file.txt");
        let result = validator.validate(&new_file);
        assert!(
            result.is_ok(),
            "Should allow new file in non-existent dir: {result:?}",
        );
    }

    #[test]
    fn blocks_symlink_escape() {
        let root = temp_workspace();
        let validator = PathValidator::new(&root).unwrap();

        // Create a symlink inside workspace pointing outside
        let link = root.join("escape_link");
        let _ = std::os::unix::fs::symlink("/etc/passwd", &link);

        // Following the symlink should be blocked
        let result = validator.validate(&link);
        assert!(
            matches!(result, Err(GatewayError::PathEscape { .. })),
            "Should block symlink escape: {result:?}",
        );

        // Cleanup
        let _ = fs::remove_file(&link);
    }

    #[test]
    fn workspace_root_must_be_absolute() {
        let result = PathValidator::new("relative/path");
        assert!(result.is_err());
    }
}
