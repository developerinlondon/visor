//! `.dockerignore` file parsing and path filtering.
//!
//! Uses the [`ignore`](::ignore) crate's gitignore glob engine to match paths
//! against `.dockerignore` patterns, filtering the build context before
//! sending it to the build engine.

use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// Files that Docker **never** excludes from the build context, even if they
/// match an ignore pattern.
const ALWAYS_INCLUDED: &[&str] = &["Dockerfile", ".dockerignore"];

/// Filters build-context paths against `.dockerignore` rules.
///
/// Docker's `.dockerignore` format is a subset of `.gitignore` with minor
/// differences (e.g. `Dockerfile` / `.dockerignore` are always included).
#[derive(Debug)]
pub struct DockerIgnore {
    matcher: Gitignore,
}

impl DockerIgnore {
    /// Parse `.dockerignore` content into a filter.
    ///
    /// Each non-empty, non-comment line is treated as a glob pattern.
    /// Lines starting with `!` negate (re-include) a previous exclusion.
    ///
    /// # Errors
    ///
    /// Returns an error if the gitignore glob builder fails to compile
    /// the pattern set.
    pub fn new(content: &str) -> anyhow::Result<Self> {
        let mut builder = GitignoreBuilder::new("");
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            builder
                .add_line(None, trimmed)
                .map_err(|e| anyhow::anyhow!("bad .dockerignore pattern: {e}"))?;
        }
        let matcher = builder
            .build()
            .map_err(|e| anyhow::anyhow!("failed to compile .dockerignore patterns: {e}"))?;
        Ok(Self { matcher })
    }

    /// Returns `true` if `path` should be **excluded** from the build context.
    ///
    /// `Dockerfile` and `.dockerignore` are never excluded, regardless of
    /// patterns.
    #[must_use]
    pub fn is_excluded(&self, path: &str) -> bool {
        // Docker convention: these two are always sent to the builder.
        if path_is_always_included(path) {
            return false;
        }

        // Use `matched_path_or_any_parents` so that a pattern like
        // `node_modules` also excludes `node_modules/package.json`.
        self.matcher
            .matched_path_or_any_parents(path, false)
            .is_ignore()
    }

    /// Filter a list of paths, returning only those that are **included**
    /// (i.e. not excluded by any pattern).
    #[must_use]
    pub fn filter_paths<'a>(&self, paths: &[&'a str]) -> Vec<&'a str> {
        paths
            .iter()
            .copied()
            .filter(|p| !self.is_excluded(p))
            .collect()
    }
}

/// Returns `true` if the path refers to a file that must always be included.
fn path_is_always_included(full_path: &str) -> bool {
    let file_name = Path::new(full_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(full_path);
    // Only protect the bare filename at the context root (no directory prefix).
    for &protected in ALWAYS_INCLUDED {
        if full_path == protected || (file_name == protected && !full_path.contains('/')) {
            return true;
        }
    }
    false
}

#[cfg(test)]
#[path = "ignore_test.rs"]
mod tests;
