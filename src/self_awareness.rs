//! Embedded Spacebot self-knowledge for introspection and diagnostics.
//!
//! This module bundles key docs into the binary and exposes helpers used by:
//! - cortex chat prompt enrichment (AGENTS + changelog + live config snapshot)
//! - `spacebot_docs` tool (on-demand document retrieval)
//! - `config_inspect` tool (redacted runtime config visibility)

use crate::config::{McpTransport, RuntimeConfig};
use rust_embed::Embed;
use serde::Serialize;
use serde_json::json;
use std::sync::OnceLock;

#[derive(Embed)]
#[folder = "docs/content/"]
struct ContentDocsAssets;

const AGENTS_DOC: &str = include_str!("../AGENTS.md");
const README_DOC: &str = include_str!("../README.md");
const CHANGELOG_DOC: &str = include_str!("../CHANGELOG.md");
const DOCS_README_DOC: &str = include_str!("../docs/README.md");
const DOCS_DOCKER_DOC: &str = include_str!("../docs/docker.md");
const DOCS_METRICS_DOC: &str = include_str!("../docs/metrics.md");
const CORTEX_AGENTS_MAX_LINES: usize = 220;
const CORTEX_AGENTS_MAX_CHARS: usize = 24_000;
const CORTEX_CHANGELOG_MAX_LINES: usize = 220;
const CORTEX_CHANGELOG_MAX_CHARS: usize = 24_000;

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddedDocSummary {
    pub id: String,
    pub title: String,
    pub path: String,
    pub section: String,
    pub line_count: usize,
}

#[derive(Debug, Clone)]
pub struct EmbeddedDoc {
    pub summary: EmbeddedDocSummary,
    pub content: String,
}

/// Full embedded `AGENTS.md` content.
pub fn agents_manifest() -> &'static str {
    AGENTS_DOC
}

/// Prompt-sized `AGENTS.md` content for cortex chat.
pub fn agents_manifest_for_prompt() -> String {
    truncate_for_prompt(
        AGENTS_DOC,
        CORTEX_AGENTS_MAX_LINES,
        CORTEX_AGENTS_MAX_CHARS,
        "AGENTS.md",
    )
}

/// Most recent release notes extracted from `CHANGELOG.md`.
pub fn changelog_highlights() -> String {
    let highlights = latest_release_notes(CHANGELOG_DOC, 3).unwrap_or_else(|| {
        CHANGELOG_DOC
            .lines()
            .take(200)
            .collect::<Vec<_>>()
            .join("\n")
    });

    truncate_for_prompt(
        &highlights,
        CORTEX_CHANGELOG_MAX_LINES,
        CORTEX_CHANGELOG_MAX_CHARS,
        "CHANGELOG.md",
    )
}

/// Pretty JSON snapshot of the current live runtime config (redacted).
pub fn runtime_snapshot_pretty(agent_id: &str, runtime_config: &RuntimeConfig) -> String {
    serde_json::to_string_pretty(&runtime_snapshot_value(agent_id, runtime_config)).unwrap_or_else(
        |error| format!("{{\"error\":\"failed to serialize runtime snapshot: {error}\"}}"),
    )
}

/// Structured runtime snapshot used by cortex prompting and config inspection.
pub fn runtime_snapshot_value(agent_id: &str, runtime_config: &RuntimeConfig) -> serde_json::Value {
    let routing = runtime_config.routing.load();
    let compaction = runtime_config.compaction.load();
    let memory_persistence = runtime_config.memory_persistence.load();
    let coalesce = runtime_config.coalesce.load();
    let ingestion = runtime_config.ingestion.load();
    let cortex = runtime_config.cortex.load();
    let warmup = runtime_config.warmup.load();
    let warmup_status = runtime_config.warmup_status.load();
    let browser = runtime_config.browser_config.load();
    let sandbox = runtime_config.sandbox.load();
    let opencode = runtime_config.opencode.load();
    let mcp_servers = runtime_config
        .mcp
        .load()
        .iter()
        .map(|server| {
            let transport = match &server.transport {
                McpTransport::Stdio { command, args, env } => {
                    let mut env_keys = env.keys().cloned().collect::<Vec<_>>();
                    env_keys.sort();
                    json!({
                        "kind": "stdio",
                        "command": command,
                        "args_count": args.len(),
                        "env_keys": env_keys,
                    })
                }
                McpTransport::Http { url, headers } => {
                    let mut header_keys = headers.keys().cloned().collect::<Vec<_>>();
                    header_keys.sort();
                    json!({
                        "kind": "http",
                        "url": url,
                        "header_keys": header_keys,
                    })
                }
            };

            json!({
                "name": server.name,
                "enabled": server.enabled,
                "transport": transport,
            })
        })
        .collect::<Vec<_>>();

    let readiness = runtime_config.work_readiness();
    let memory_bulletin = runtime_config.memory_bulletin.load();
    let secrets = runtime_config.secrets.load();
    let secrets_snapshot = if let Some(store) = secrets.as_ref() {
        match store.status(false) {
            Ok(status) => json!({
                "configured": true,
                "state": status.state.to_string(),
                "encrypted": status.encrypted,
                "secret_count": status.secret_count,
                "system_count": status.system_count,
                "tool_count": status.tool_count,
            }),
            Err(error) => json!({
                "configured": true,
                "error": error.to_string(),
            }),
        }
    } else {
        json!({
            "configured": false,
        })
    };

    json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "agent_id": agent_id,
        "binary_version": crate::update::CURRENT_VERSION,
        "deployment": deployment_label(crate::update::Deployment::detect()),
        "paths": {
            "instance_dir": runtime_config.instance_dir.display().to_string(),
            "workspace_dir": runtime_config.workspace_dir.display().to_string(),
        },
        "routing": {
            "channel": routing.channel,
            "branch": routing.branch,
            "worker": routing.worker,
            "compactor": routing.compactor,
            "cortex": routing.cortex,
            "voice": routing.voice,
            "rate_limit_cooldown_secs": routing.rate_limit_cooldown_secs,
        },
        "limits": {
            "max_turns": **runtime_config.max_turns.load(),
            "branch_max_turns": **runtime_config.branch_max_turns.load(),
            "context_window": **runtime_config.context_window.load(),
            "max_concurrent_branches": **runtime_config.max_concurrent_branches.load(),
            "max_concurrent_workers": **runtime_config.max_concurrent_workers.load(),
            "history_backfill_count": **runtime_config.history_backfill_count.load(),
        },
        "compaction": {
            "background_threshold": compaction.background_threshold,
            "aggressive_threshold": compaction.aggressive_threshold,
            "emergency_threshold": compaction.emergency_threshold,
        },
        "memory_persistence": {
            "enabled": memory_persistence.enabled,
            "message_interval": memory_persistence.message_interval,
        },
        "coalesce": {
            "enabled": coalesce.enabled,
            "debounce_ms": coalesce.debounce_ms,
            "max_wait_ms": coalesce.max_wait_ms,
            "min_messages": coalesce.min_messages,
            "multi_user_only": coalesce.multi_user_only,
        },
        "ingestion": {
            "enabled": ingestion.enabled,
            "poll_interval_secs": ingestion.poll_interval_secs,
            "chunk_size": ingestion.chunk_size,
        },
        "cortex": {
            "tick_interval_secs": cortex.tick_interval_secs,
            "worker_timeout_secs": cortex.worker_timeout_secs,
            "branch_timeout_secs": cortex.branch_timeout_secs,
            "circuit_breaker_threshold": cortex.circuit_breaker_threshold,
            "bulletin_interval_secs": cortex.bulletin_interval_secs,
            "bulletin_max_words": cortex.bulletin_max_words,
            "bulletin_max_turns": cortex.bulletin_max_turns,
            "association_interval_secs": cortex.association_interval_secs,
            "association_similarity_threshold": cortex.association_similarity_threshold,
            "association_updates_threshold": cortex.association_updates_threshold,
            "association_max_per_pass": cortex.association_max_per_pass,
        },
        "warmup": {
            "enabled": warmup.enabled,
            "eager_embedding_load": warmup.eager_embedding_load,
            "refresh_secs": warmup.refresh_secs,
            "startup_delay_secs": warmup.startup_delay_secs,
            "state": warmup_status.state,
            "embedding_ready": warmup_status.embedding_ready,
            "last_refresh_unix_ms": warmup_status.last_refresh_unix_ms,
            "last_error": warmup_status.last_error,
            "bulletin_age_secs": warmup_status.bulletin_age_secs,
        },
        "work_readiness": {
            "ready": readiness.ready,
            "reason": readiness.reason.map(|reason| reason.as_str()),
            "warmup_state": readiness.warmup_state,
            "embedding_ready": readiness.embedding_ready,
            "bulletin_age_secs": readiness.bulletin_age_secs,
            "stale_after_secs": readiness.stale_after_secs,
        },
        "browser": {
            "enabled": browser.enabled,
            "headless": browser.headless,
            "evaluate_enabled": browser.evaluate_enabled,
            "executable_path": browser.executable_path,
            "screenshot_dir": browser
                .screenshot_dir
                .as_ref()
                .map(|path| path.display().to_string()),
            "chrome_cache_dir": browser.chrome_cache_dir.display().to_string(),
        },
        "sandbox": {
            "mode": match sandbox.mode {
                crate::sandbox::SandboxMode::Enabled => "enabled",
                crate::sandbox::SandboxMode::Disabled => "disabled",
            },
            "writable_paths": sandbox
                .writable_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
            "passthrough_env": sandbox.passthrough_env,
        },
        "opencode": {
            "enabled": opencode.enabled,
            "path": opencode.path,
            "max_servers": opencode.max_servers,
            "server_startup_timeout_secs": opencode.server_startup_timeout_secs,
            "max_restart_retries": opencode.max_restart_retries,
            "permissions": opencode.permissions,
        },
        "mcp_servers": mcp_servers,
        "brave_search": {
            "configured": runtime_config.brave_search_key.load().is_some(),
        },
        "timezones": {
            "cron_timezone": runtime_config.cron_timezone.load().as_ref().clone(),
            "user_timezone": runtime_config.user_timezone.load().as_ref().clone(),
        },
        "bulletin": {
            "is_empty": memory_bulletin.trim().is_empty(),
            "char_count": memory_bulletin.len(),
        },
        "secrets": secrets_snapshot,
    })
}

/// List all embedded docs with optional text filter.
pub fn list_embedded_docs(filter: Option<&str>) -> Vec<EmbeddedDocSummary> {
    let mut docs = docs_catalog()
        .iter()
        .map(|doc| doc.summary.clone())
        .collect::<Vec<_>>();

    if let Some(raw_filter) = filter {
        let filter = normalize_lookup(raw_filter);
        if !filter.is_empty() {
            docs.retain(|doc| {
                doc.id.to_ascii_lowercase().contains(&filter)
                    || doc.path.to_ascii_lowercase().contains(&filter)
                    || doc.title.to_ascii_lowercase().contains(&filter)
                    || doc.section.to_ascii_lowercase().contains(&filter)
            });
        }
    }

    docs
}

/// Search docs by query string (ID, title, section, or path).
pub fn search_embedded_docs(query: &str) -> Vec<EmbeddedDocSummary> {
    list_embedded_docs(Some(query))
}

/// Get a doc by ID/path, with lightweight fuzzy matching.
///
/// Matching strategy:
/// 1. exact ID (case-insensitive)
/// 2. exact path (case-insensitive)
/// 3. single fuzzy hit on ID/path/title contains
pub fn get_embedded_doc(query: &str) -> Option<EmbeddedDoc> {
    let normalized = normalize_lookup(query);
    if normalized.is_empty() {
        return None;
    }

    if let Some(doc) = docs_catalog()
        .iter()
        .find(|doc| normalize_lookup(&doc.summary.id) == normalized)
    {
        return Some(doc.clone());
    }

    if let Some(doc) = docs_catalog()
        .iter()
        .find(|doc| normalize_lookup(&doc.summary.path) == normalized)
    {
        return Some(doc.clone());
    }

    let mut matches = docs_catalog()
        .iter()
        .filter(|doc| {
            let id = doc.summary.id.to_ascii_lowercase();
            let path = doc.summary.path.to_ascii_lowercase();
            let title = doc.summary.title.to_ascii_lowercase();
            id.contains(&normalized) || path.contains(&normalized) || title.contains(&normalized)
        })
        .cloned()
        .collect::<Vec<_>>();

    if matches.len() == 1 {
        return matches.pop();
    }

    None
}

fn docs_catalog() -> &'static [EmbeddedDoc] {
    static CATALOG: OnceLock<Vec<EmbeddedDoc>> = OnceLock::new();
    CATALOG.get_or_init(build_docs_catalog).as_slice()
}

fn build_docs_catalog() -> Vec<EmbeddedDoc> {
    let mut docs = Vec::new();

    push_static_doc(
        &mut docs,
        "agents",
        "AGENTS",
        "AGENTS.md",
        "repo",
        AGENTS_DOC.to_string(),
    );
    push_static_doc(
        &mut docs,
        "readme",
        "README",
        "README.md",
        "repo",
        README_DOC.to_string(),
    );
    push_static_doc(
        &mut docs,
        "changelog",
        "Changelog",
        "CHANGELOG.md",
        "repo",
        CHANGELOG_DOC.to_string(),
    );
    push_static_doc(
        &mut docs,
        "docs/readme",
        "Docs README",
        "docs/README.md",
        "docs",
        DOCS_README_DOC.to_string(),
    );
    push_static_doc(
        &mut docs,
        "docs/docker",
        "Docker Guide",
        "docs/docker.md",
        "docs",
        DOCS_DOCKER_DOC.to_string(),
    );
    push_static_doc(
        &mut docs,
        "docs/metrics",
        "Metrics",
        "docs/metrics.md",
        "docs",
        DOCS_METRICS_DOC.to_string(),
    );

    for relative in ContentDocsAssets::iter() {
        let relative = relative.as_ref();
        if !is_markdown(relative) {
            continue;
        }

        let Some(file) = ContentDocsAssets::get(relative) else {
            continue;
        };

        let content = String::from_utf8_lossy(file.data.as_ref()).into_owned();
        let title = extract_doc_title(&content, relative);
        let id = product_doc_id(relative);
        let path = format!("docs/content/{relative}");

        push_static_doc(&mut docs, &id, &title, &path, "product_docs", content);
    }

    docs.sort_by(|left, right| left.summary.id.cmp(&right.summary.id));
    docs
}

fn push_static_doc(
    docs: &mut Vec<EmbeddedDoc>,
    id: &str,
    title: &str,
    path: &str,
    section: &str,
    content: String,
) {
    let line_count = content.lines().count();
    docs.push(EmbeddedDoc {
        summary: EmbeddedDocSummary {
            id: id.to_string(),
            title: title.to_string(),
            path: path.to_string(),
            section: section.to_string(),
            line_count,
        },
        content,
    });
}

fn latest_release_notes(changelog: &str, max_releases: usize) -> Option<String> {
    if max_releases == 0 {
        return None;
    }

    let is_release_heading = |line: &str| {
        let heading = line.strip_prefix("## ").map(str::trim).unwrap_or("");
        let version = heading
            .strip_prefix('v')
            .or_else(|| heading.strip_prefix('V'))
            .unwrap_or(heading);
        let mut parts = version.split('.');
        let major = parts.next();
        let minor = parts.next();
        let patch = parts.next();
        let rest = parts.next();

        major.is_some_and(|part| part.chars().all(|c| c.is_ascii_digit()) && !part.is_empty())
            && minor
                .is_some_and(|part| part.chars().all(|c| c.is_ascii_digit()) && !part.is_empty())
            && patch
                .is_some_and(|part| part.chars().all(|c| c.is_ascii_digit()) && !part.is_empty())
            && rest.is_none()
    };

    let mut sections = Vec::new();
    let mut current = Vec::new();
    let mut in_release = false;

    for line in changelog.lines() {
        if line.starts_with("## ") && is_release_heading(line) {
            if !current.is_empty() {
                sections.push(current.join("\n"));
                current.clear();
            }
            in_release = true;
        }

        if in_release {
            current.push(line);
        }
    }

    if !current.is_empty() {
        sections.push(current.join("\n"));
    }

    if sections.is_empty() {
        None
    } else {
        Some(
            sections
                .into_iter()
                .take(max_releases)
                .collect::<Vec<_>>()
                .join("\n\n"),
        )
    }
}

fn extract_doc_title(content: &str, relative_path: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("title:") {
            let title = title.trim().trim_matches('"').trim_matches(char::from(39));
            if !title.is_empty() {
                return title.to_string();
            }
        }
        if let Some(title) = trimmed.strip_prefix("# ") {
            let title = title.trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
    }

    let fallback = remove_extension(relative_path)
        .rsplit('/')
        .next()
        .unwrap_or(relative_path)
        .replace(['-', '_'], " ");
    if fallback.is_empty() {
        "Untitled".to_string()
    } else {
        fallback
    }
}

fn product_doc_id(relative_path: &str) -> String {
    let mut normalized = normalize_doc_path(remove_extension(relative_path));
    if let Some(stripped) = normalized.strip_prefix("docs/") {
        normalized = stripped.to_string();
    }
    if normalized == "index" {
        normalized = "home".to_string();
    } else if let Some(prefix) = normalized.strip_suffix("/index") {
        normalized = prefix.to_string();
    }
    format!("docs/{normalized}")
}

fn normalize_doc_path(path: &str) -> String {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(strip_group_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn strip_group_segment(segment: &str) -> String {
    if segment.starts_with('(') && segment.ends_with(')') && segment.len() > 2 {
        return segment[1..segment.len() - 1].to_string();
    }
    segment.to_string()
}

fn remove_extension(path: &str) -> &str {
    path.strip_suffix(".mdx")
        .or_else(|| path.strip_suffix(".md"))
        .unwrap_or(path)
}

fn is_markdown(path: &str) -> bool {
    path.ends_with(".md") || path.ends_with(".mdx")
}

fn normalize_lookup(value: &str) -> String {
    value.trim().trim_start_matches('/').to_ascii_lowercase()
}

fn truncate_for_prompt(content: &str, max_lines: usize, max_chars: usize, label: &str) -> String {
    let limited_lines = content.lines().take(max_lines).collect::<Vec<_>>();
    let mut output = limited_lines.join("\n");
    let mut truncated = content.lines().count() > limited_lines.len();

    if output.chars().count() > max_chars {
        output = output.chars().take(max_chars).collect::<String>();
        truncated = true;
    }

    if truncated {
        output.push_str(&format!(
            "\n\n_[{label} truncated in prompt; use `spacebot_docs` for full content.]_"
        ));
    }

    output
}

fn deployment_label(deployment: crate::update::Deployment) -> &'static str {
    match deployment {
        crate::update::Deployment::Docker => "docker",
        crate::update::Deployment::Hosted => "hosted",
        crate::update::Deployment::Native => "native",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn latest_release_notes_returns_top_release_sections() {
        let changelog = "# Changelog\n\nIntro\n\n## v3.0.0\nA\n\n## What's Changed\n- item\n\n## v2.0.0\nB\n\n## v1.0.0\nC\n";
        let section = latest_release_notes(changelog, 2).expect("sections should exist");
        assert!(section.contains("## v3.0.0"));
        assert!(section.contains("## v2.0.0"));
        assert!(!section.contains("## v1.0.0"));
        assert!(section.contains("## What's Changed"));
    }

    #[test]
    fn truncate_for_prompt_adds_notice_when_content_is_trimmed() {
        let text = "line1\nline2\nline3\nline4";
        let output = truncate_for_prompt(text, 2, 500, "test.md");
        assert!(output.contains("line1\nline2"));
        assert!(output.contains("truncated in prompt"));
    }

    #[test]
    fn normalize_doc_path_strips_group_segments() {
        assert_eq!(
            normalize_doc_path("(core)/architecture"),
            "core/architecture"
        );
        assert_eq!(
            normalize_doc_path("(messaging)/discord-setup"),
            "messaging/discord-setup"
        );
    }

    #[test]
    fn product_doc_id_maps_index_to_home_or_section_root() {
        assert_eq!(product_doc_id("index.mdx"), "docs/home");
        assert_eq!(product_doc_id("(core)/index.mdx"), "docs/core");
        assert_eq!(product_doc_id("(core)/cortex.mdx"), "docs/core/cortex");
        assert_eq!(product_doc_id("docs/(core)/cortex.mdx"), "docs/core/cortex");
    }

    #[test]
    fn product_docs_catalog_matches_embedded_content_assets() {
        let expected = ContentDocsAssets::iter()
            .map(|relative| relative.to_string())
            .filter(|relative| is_markdown(relative))
            .map(|relative| format!("docs/content/{relative}"))
            .collect::<BTreeSet<_>>();

        let actual = list_embedded_docs(None)
            .into_iter()
            .filter(|doc| doc.section == "product_docs")
            .map(|doc| doc.path)
            .collect::<BTreeSet<_>>();

        let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
        let unexpected = actual.difference(&expected).cloned().collect::<Vec<_>>();

        assert!(
            missing.is_empty() && unexpected.is_empty(),
            "product docs mismatch\nmissing: {:?}\nunexpected: {:?}",
            missing,
            unexpected
        );
    }

    #[test]
    fn bundled_docs_inventory_is_accessible() {
        let mut docs = list_embedded_docs(None);
        docs.sort_by(|left, right| left.id.cmp(&right.id));
        assert!(
            !docs.is_empty(),
            "embedded docs catalog should not be empty"
        );

        println!("bundled docs: {}", docs.len());
        for doc in &docs {
            println!("{} | {} | {}", doc.section, doc.id, doc.path);
        }
    }
}
