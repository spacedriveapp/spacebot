//! Phase 1: Filesystem walk with .gitignore / .spacebotignore respect.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::codegraph::lang;
use crate::codegraph::types::CodeGraphConfig;

/// Files larger than this are skipped during the walk — they are almost
/// always generated, minified, or vendored code that tree-sitter either
/// crashes on or wastes time parsing.
const MAX_FILE_SIZE_BYTES: u64 = 512 * 1024;

/// Environment variable that, when set to any value, disables `.gitignore`
/// (and `.git/info/exclude` / global gitignore) parsing during the walk.
/// Escape hatch for repos whose `.gitignore` accidentally excludes files
/// you want indexed.
const NO_GITIGNORE_ENV: &str = "SPACEBOT_NO_GITIGNORE";

/// Result of walking a project directory, including metadata about
/// which ignore rules were applied. Surfaced to the user through the
/// pipeline's Extracting-phase progress message.
#[derive(Debug, Default, Clone)]
pub struct WalkOutcome {
    /// The source files the pipeline should index.
    pub files: Vec<PathBuf>,
    /// Path to the `.spacebotignore` file that was loaded, if any.
    pub spacebotignore_loaded: Option<PathBuf>,
    /// True if `SPACEBOT_NO_GITIGNORE` was set and `.gitignore` parsing
    /// was bypassed for this walk.
    pub gitignore_bypassed: bool,
    /// Count of files dropped because they exceeded [`MAX_FILE_SIZE_BYTES`].
    pub oversized_skipped: usize,
}

/// Emit an incremental progress update every N files walked. The walker
/// doesn't know the total file count up front, so the percentage is
/// clamped below 1.0 (the final 1.0 tick is fired by the pipeline caller
/// after `walk_project` returns).
const WALK_PROGRESS_INTERVAL: usize = 500;

/// Walk the project directory and return all parseable source files
/// along with metadata about which ignore rules were applied.
///
/// Respects `.gitignore` and `.spacebotignore`, skips binary/build
/// artifacts and oversized files, and optionally filters by language if
/// `config.language_filter` is set.
///
/// `.spacebotignore` is read **only from the project root** (non-recursive).
/// Its patterns use the standard gitignore syntax and are resolved relative
/// to the root. Set the `SPACEBOT_NO_GITIGNORE` env var to bypass
/// `.gitignore` entirely.
///
/// If `progress_fn` is supplied, the walker emits an incremental update
/// every [`WALK_PROGRESS_INTERVAL`] files so huge repos don't appear to
/// freeze during the walk.
pub async fn walk_project(
    root_path: &Path,
    config: &CodeGraphConfig,
    progress_fn: Option<&super::ProgressFn>,
) -> Result<WalkOutcome> {
    let root = root_path.to_path_buf();
    let language_filter = config.language_filter.clone();
    // Clone the Arc so the closure owns it across the blocking boundary.
    let progress: Option<super::ProgressFn> = progress_fn.cloned();

    // Run the walk on a blocking thread since `ignore` crate is synchronous.
    let outcome = tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        let mut oversized_skipped: usize = 0;

        // Escape hatch: allow users to bypass gitignore rules.
        let gitignore_bypassed = std::env::var_os(NO_GITIGNORE_ENV).is_some();
        let respect_gitignore = !gitignore_bypassed;
        if gitignore_bypassed {
            tracing::info!(
                env = NO_GITIGNORE_ENV,
                "bypassing .gitignore rules for code graph walk"
            );
        }

        let mut builder = ignore::WalkBuilder::new(&root);
        builder
            .hidden(true) // skip hidden files
            .git_ignore(respect_gitignore)
            .git_global(respect_gitignore)
            .git_exclude(respect_gitignore)
            .follow_links(false)
            .max_depth(None);

        // Load `.spacebotignore` from the repo root ONLY (non-recursive).
        // `add_ignore` treats the file's parent dir as the base for
        // pattern matching, so patterns are anchored to `root`.
        let spacebot_ignore_candidate = root.join(".spacebotignore");
        let spacebotignore_loaded = if spacebot_ignore_candidate.is_file() {
            match builder.add_ignore(&spacebot_ignore_candidate) {
                Some(err) => {
                    tracing::warn!(
                        %err,
                        path = %spacebot_ignore_candidate.display(),
                        "failed to parse .spacebotignore"
                    );
                    None
                }
                None => {
                    tracing::info!(
                        path = %spacebot_ignore_candidate.display(),
                        "loaded .spacebotignore"
                    );
                    Some(spacebot_ignore_candidate.clone())
                }
            }
        } else {
            tracing::debug!(
                path = %spacebot_ignore_candidate.display(),
                "no .spacebotignore found at project root"
            );
            None
        };

        let walker = builder.build();

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

            // Skip binary, generated, and build artifacts.
            if is_build_artifact(&path) {
                continue;
            }

            // Check if we can determine a language for this file.
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let detected = lang::language_for_extension(ext);
            if detected.is_none() {
                continue;
            }

            // Apply language filter if set.
            if !language_filter.is_empty()
                && let Some(detected) = detected
            {
                let lang_name = detected.as_str().to_lowercase();
                if !language_filter.iter().any(|f| f.to_lowercase() == lang_name) {
                    continue;
                }
            }

            // Size cap: drop files the parser would choke on. `entry.metadata()`
            // reuses the stat that `ignore` already performed, so this is cheap.
            if let Ok(meta) = entry.metadata()
                && meta.len() > MAX_FILE_SIZE_BYTES
            {
                oversized_skipped += 1;
                tracing::debug!(
                    file = %path.display(),
                    size = meta.len(),
                    "skipping large file (likely generated/vendored)"
                );
                continue;
            }

            files.push(path);

            if let Some(ref pf) = progress
                && files.len().is_multiple_of(WALK_PROGRESS_INTERVAL)
            {
                // We don't know the final total, so clamp the reported
                // percentage below 1.0. The pipeline caller emits the
                // final 1.0 tick after this function returns.
                pf(
                    0.5,
                    &format!("Walking filesystem ({} files found)", files.len()),
                    &super::PhaseResult::default(),
                );
            }
        }

        if oversized_skipped > 0 {
            tracing::info!(
                count = oversized_skipped,
                max_kb = MAX_FILE_SIZE_BYTES / 1024,
                "skipped oversized files during walk"
            );
        }

        WalkOutcome {
            files,
            spacebotignore_loaded,
            gitignore_bypassed,
            oversized_skipped,
        }
    })
    .await?;

    Ok(outcome)
}

/// Check if a path looks like a build artifact, generated file, or any
/// other non-source file that should be excluded from indexing.
fn is_build_artifact(path: &Path) -> bool {
    // Walk path components and reject any that match a skip directory.
    // Using `Component::Normal` is cross-platform and avoids the fragile
    // `/name/` vs `\name\` substring matching we used before.
    for component in path.components() {
        if let std::path::Component::Normal(os) = component
            && let Some(name) = os.to_str()
            && SKIP_DIRS.contains(&name)
        {
            return true;
        }
    }

    let file_name = match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => return false,
    };
    let lower = file_name.to_ascii_lowercase();

    // Exact filename match (lock files, platform cruft).
    if SKIP_FILENAMES.contains(&lower.as_str()) {
        return true;
    }

    // Single-extension match (.png, .exe, ...).
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext_lower = ext.to_ascii_lowercase();
        if SKIP_EXTENSIONS.contains(&ext_lower.as_str()) {
            return true;
        }
    }

    // Compound / substring patterns that the single-extension check misses.
    // E.g. `foo.min.js` has extension `js` which is a valid source ext, so
    // we need a separate check to catch it.
    if lower.ends_with(".d.ts")
        || lower.ends_with(".min.js")
        || lower.ends_with(".min.css")
        || lower.contains(".bundle.")
        || lower.contains(".chunk.")
        || lower.contains(".generated.")
    {
        return true;
    }

    false
}

/// Directory basenames that should never be indexed.
const SKIP_DIRS: &[&str] = &[
    // Version control
    ".git",
    ".svn",
    ".hg",
    ".bzr",
    // IDEs & editors
    ".idea",
    ".vscode",
    ".vs",
    ".eclipse",
    ".settings",
    // JS/TS dependencies
    "node_modules",
    "bower_components",
    "jspm_packages",
    // Generic vendor
    "vendor",
    // Python
    "venv",
    ".venv",
    "env",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    "site-packages",
    ".tox",
    "eggs",
    ".eggs",
    "wheels",
    "sdist",
    "lib64",
    "parts",
    // Build outputs
    "dist",
    "build",
    "out",
    "output",
    "bin",
    "obj",
    "target",
    ".next",
    ".nuxt",
    ".output",
    ".vercel",
    ".netlify",
    ".serverless",
    "_build",
    ".parcel-cache",
    ".turbo",
    ".svelte-kit",
    // Test & coverage
    "coverage",
    ".nyc_output",
    "htmlcov",
    "__tests__",
    "__mocks__",
    ".jest",
    // Logs & temp
    "logs",
    "log",
    "tmp",
    "temp",
    "cache",
    ".cache",
    ".tmp",
    ".temp",
    // Generated
    ".generated",
    "generated",
    "auto-generated",
    ".terraform",
    // CI/CD metadata
    ".husky",
    ".github",
    ".circleci",
    ".gitlab",
    // Test assets
    "fixtures",
    "snapshots",
    "__snapshots__",
    // Rust / Go misc
    "pkg",
];

/// File extensions (without the leading dot) to skip.
const SKIP_EXTENSIONS: &[&str] = &[
    // Images
    "png", "jpg", "jpeg", "gif", "svg", "ico", "webp", "bmp", "tiff", "tif", "psd", "ai", "sketch",
    "fig", "xd",
    // Archives
    "zip", "tar", "gz", "rar", "7z", "bz2", "xz", "tgz",
    // Binary / compiled
    "exe", "dll", "so", "dylib", "a", "lib", "o", "obj", "class", "jar", "war", "ear", "pyc",
    "pyo", "pyd", "beam", "wasm", "node",
    // Documents
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp",
    // Media
    "mp4", "mp3", "wav", "mov", "avi", "mkv", "flv", "wmv", "ogg", "webm", "flac", "aac", "m4a",
    // Fonts
    "woff", "woff2", "ttf", "eot", "otf",
    // Databases
    "db", "sqlite", "sqlite3", "mdb", "accdb",
    // Source maps
    "map",
    // Lock files (by extension)
    "lock",
    // Certs & keys (never index secrets!)
    "pem", "key", "crt", "cer", "p12", "pfx",
    // Data blobs
    "csv", "tsv", "parquet", "avro", "feather", "npy", "npz", "pkl", "pickle", "h5", "hdf5",
    // Misc binary
    "bin", "dat", "data", "raw", "iso", "img", "dmg",
];

/// Exact (lowercased) filenames to skip. Mostly lock files whose
/// extension alone doesn't identify them, plus OS cruft.
const SKIP_FILENAMES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "cargo.lock",
    "gemfile.lock",
    "composer.lock",
    "poetry.lock",
    "go.sum",
    "thumbs.db",
    ".ds_store",
];
