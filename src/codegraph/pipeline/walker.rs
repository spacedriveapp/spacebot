//! Phase 1: Filesystem walk with .gitignore respect.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::codegraph::types::CodeGraphConfig;
use crate::codegraph::languages;

/// Walk the project directory and return all parseable source files.
///
/// Respects `.gitignore`, skips binary/build artifacts, and optionally
/// filters by language if `config.language_filter` is set.
pub async fn walk_project(root_path: &Path, config: &CodeGraphConfig) -> Result<Vec<PathBuf>> {
    let root = root_path.to_path_buf();
    let language_filter = config.language_filter.clone();

    // Run the walk on a blocking thread since `ignore` crate is synchronous.
    let files = tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();

        let walker = ignore::WalkBuilder::new(&root)
            .hidden(true) // skip hidden files
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .follow_links(false)
            .max_depth(None)
            .build();

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    tracing::warn!(%err, "skipping directory entry");
                    continue;
                }
            };

            // Only process files (not directories).
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }

            let path = entry.path().to_path_buf();

            // Skip binary and build artifacts.
            if is_build_artifact(&path) {
                continue;
            }

            // Check if we can determine a language for this file.
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let lang = languages::language_for_extension(ext);
            if lang.is_none() {
                continue;
            }

            // Apply language filter if set.
            if !language_filter.is_empty() {
                if let Some(lang) = lang {
                    let lang_name = lang.as_str().to_lowercase();
                    if !language_filter.iter().any(|f| f.to_lowercase() == lang_name) {
                        continue;
                    }
                }
            }

            files.push(path);
        }

        files
    })
    .await?;

    Ok(files)
}

/// Check if a path looks like a build artifact or generated file.
fn is_build_artifact(path: &Path) -> bool {
    let path_str = path.to_string_lossy();

    // Common build/generated directories.
    let skip_dirs = [
        "node_modules",
        "target",
        "dist",
        "build",
        ".git",
        "__pycache__",
        ".tox",
        ".mypy_cache",
        ".pytest_cache",
        "vendor",
        ".next",
        ".nuxt",
        "coverage",
        ".turbo",
        ".vercel",
        ".output",
        "pkg",
    ];

    for dir in &skip_dirs {
        if path_str.contains(&format!("/{dir}/")) || path_str.contains(&format!("\\{dir}\\")) {
            return true;
        }
    }

    // Skip lock files, compiled outputs, etc.
    let skip_extensions = [
        "lock", "min.js", "min.css", "map", "wasm", "o", "a", "so", "dll",
        "exe", "bin", "pyc", "pyo", "class", "jar", "war",
    ];

    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if skip_extensions.contains(&ext) {
            return true;
        }
    }

    // Skip specific filenames.
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if matches!(
            name,
            "package-lock.json"
                | "yarn.lock"
                | "pnpm-lock.yaml"
                | "Cargo.lock"
                | "Gemfile.lock"
                | "composer.lock"
                | "poetry.lock"
        ) {
            return true;
        }
    }

    false
}
