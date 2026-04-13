//! Instance-wide wiki storage (SQLite).

use crate::error::{Result, WikiError};
use anyhow::Context as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Row as _, SqlitePool};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WikiPageType {
    Entity,
    Concept,
    Decision,
    Project,
    Reference,
}

impl WikiPageType {
    pub const ALL: [WikiPageType; 5] = [
        WikiPageType::Entity,
        WikiPageType::Concept,
        WikiPageType::Decision,
        WikiPageType::Project,
        WikiPageType::Reference,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WikiPageType::Entity => "entity",
            WikiPageType::Concept => "concept",
            WikiPageType::Decision => "decision",
            WikiPageType::Project => "project",
            WikiPageType::Reference => "reference",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "entity" => Some(WikiPageType::Entity),
            "concept" => Some(WikiPageType::Concept),
            "decision" => Some(WikiPageType::Decision),
            "project" => Some(WikiPageType::Project),
            "reference" => Some(WikiPageType::Reference),
            _ => None,
        }
    }
}

impl std::fmt::Display for WikiPageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WikiPage {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub page_type: String,
    pub content: String,
    pub related: Vec<String>,
    pub created_by: String,
    pub updated_by: String,
    pub version: i64,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WikiPageVersion {
    pub id: String,
    pub page_id: String,
    pub version: i64,
    pub content: String,
    pub edit_summary: Option<String>,
    pub author_type: String,
    pub author_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct CreateWikiPageInput {
    pub title: String,
    pub page_type: WikiPageType,
    pub content: String,
    pub related: Vec<String>,
    pub author_type: String,
    pub author_id: String,
    pub edit_summary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EditWikiPageInput {
    pub slug: String,
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
    pub edit_summary: Option<String>,
    pub author_type: String,
    pub author_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WikiPageSummary {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub page_type: String,
    pub version: i64,
    pub updated_at: String,
    pub updated_by: String,
}

// ---------------------------------------------------------------------------
// Slug generation
// ---------------------------------------------------------------------------

/// Derive a URL-safe slug from a title.
pub fn slugify(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

// ---------------------------------------------------------------------------
// Tolerant matching for wiki_edit
// ---------------------------------------------------------------------------

/// Apply a tolerant string replacement to `content`.
/// Returns the new content if a match was found, or an error describing the failure.
pub fn tolerant_replace(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> std::result::Result<String, String> {
    // Pass 1: exact match
    if content.contains(old_string) {
        return Ok(if replace_all {
            content.replace(old_string, new_string)
        } else {
            let count = content.matches(old_string).count();
            if count > 1 {
                return Err(format!(
                    "old_string matches {count} locations. Set replace_all=true or provide more surrounding context to make it unique."
                ));
            }
            content.replacen(old_string, new_string, 1)
        });
    }

    // Pass 2: line-trimmed match — trim each line, then compare
    let trim_lines =
        |s: &str| -> String { s.lines().map(|l| l.trim()).collect::<Vec<_>>().join("\n") };
    let content_trimmed = trim_lines(content);
    let old_trimmed = trim_lines(old_string);
    if content_trimmed.contains(&old_trimmed) {
        // Find the original span by scanning line by line
        let old_lines: Vec<&str> = old_string.lines().collect();
        let content_lines: Vec<&str> = content.lines().collect();
        if let Some(start) = find_trimmed_block(&content_lines, &old_lines) {
            let end = start + old_lines.len();
            let prefix = content_lines[..start].join("\n");
            let suffix = content_lines[end..].join("\n");
            let sep_pre = if prefix.is_empty() { "" } else { "\n" };
            let sep_suf = if suffix.is_empty() { "" } else { "\n" };
            let result = format!("{prefix}{sep_pre}{new_string}{sep_suf}{suffix}");
            return Ok(result);
        }
    }

    // Pass 3: whitespace-normalised match
    let normalise = |s: &str| -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") };
    let content_norm = normalise(content);
    let old_norm = normalise(old_string);
    if content_norm.contains(&old_norm) {
        // Best-effort: rebuild with a rough positional replacement
        // For normalised matches we fall back to a targeted line-block scan
        let old_lines: Vec<&str> = old_string.lines().collect();
        let content_lines: Vec<&str> = content.lines().collect();
        if let Some(start) = find_normalised_block(&content_lines, &old_lines) {
            let end = start + old_lines.len();
            let prefix = content_lines[..start].join("\n");
            let suffix = content_lines[end..].join("\n");
            let sep_pre = if prefix.is_empty() { "" } else { "\n" };
            let sep_suf = if suffix.is_empty() { "" } else { "\n" };
            return Ok(format!("{prefix}{sep_pre}{new_string}{sep_suf}{suffix}"));
        }
    }

    // Pass 4: indentation-flexible — strip leading whitespace from each line
    let strip_indent = |s: &str| -> String {
        s.lines()
            .map(|l| l.trim_start())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let content_stripped = strip_indent(content);
    let old_stripped = strip_indent(old_string);
    if content_stripped.contains(&old_stripped) {
        let old_lines: Vec<&str> = old_string.lines().collect();
        let content_lines: Vec<&str> = content.lines().collect();
        if let Some(start) = find_stripped_block(&content_lines, &old_lines) {
            let end = start + old_lines.len();
            let prefix = content_lines[..start].join("\n");
            let suffix = content_lines[end..].join("\n");
            let sep_pre = if prefix.is_empty() { "" } else { "\n" };
            let sep_suf = if suffix.is_empty() { "" } else { "\n" };
            return Ok(format!("{prefix}{sep_pre}{new_string}{sep_suf}{suffix}"));
        }
    }

    // Pass 5: block anchor match — anchor on first/last lines, score interior
    let old_lines: Vec<&str> = old_string.lines().collect();
    if old_lines.len() >= 2 {
        let content_lines: Vec<&str> = content.lines().collect();
        if let Some(start) = find_anchor_block(&content_lines, &old_lines) {
            let end = start + old_lines.len();
            if end <= content_lines.len() {
                let prefix = content_lines[..start].join("\n");
                let suffix = content_lines[end..].join("\n");
                let sep_pre = if prefix.is_empty() { "" } else { "\n" };
                let sep_suf = if suffix.is_empty() { "" } else { "\n" };
                return Ok(format!("{prefix}{sep_pre}{new_string}{sep_suf}{suffix}"));
            }
        }
    }

    Err(
        "old_string not found. Try providing more surrounding context, or read the page first to confirm the exact text."
            .to_string(),
    )
}

fn find_trimmed_block(content_lines: &[&str], old_lines: &[&str]) -> Option<usize> {
    let n = old_lines.len();
    'outer: for i in 0..content_lines.len().saturating_sub(n - 1) {
        for (j, old_line) in old_lines.iter().enumerate() {
            if content_lines[i + j].trim() != old_line.trim() {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

fn find_normalised_block(content_lines: &[&str], old_lines: &[&str]) -> Option<usize> {
    let norm = |s: &str| -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") };
    let n = old_lines.len();
    'outer: for i in 0..content_lines.len().saturating_sub(n - 1) {
        for (j, old_line) in old_lines.iter().enumerate() {
            if norm(content_lines[i + j]) != norm(old_line) {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

fn find_stripped_block(content_lines: &[&str], old_lines: &[&str]) -> Option<usize> {
    let n = old_lines.len();
    'outer: for i in 0..content_lines.len().saturating_sub(n - 1) {
        for (j, old_line) in old_lines.iter().enumerate() {
            if content_lines[i + j].trim_start() != old_line.trim_start() {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

/// Levenshtein distance for anchor scoring.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in dp.iter_mut().enumerate().take(m + 1) {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate().take(n + 1) {
        *cell = j;
    }
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if a[i - 1] == b[j - 1] {
                dp[i - 1][j - 1]
            } else {
                1 + dp[i - 1][j - 1].min(dp[i - 1][j]).min(dp[i][j - 1])
            };
        }
    }
    dp[m][n]
}

fn line_similarity(a: &str, b: &str) -> f64 {
    let a = a.trim();
    let b = b.trim();
    if a == b {
        return 1.0;
    }
    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 1.0;
    }
    let dist = levenshtein(a, b);
    1.0 - (dist as f64 / max_len as f64)
}

fn find_anchor_block(content_lines: &[&str], old_lines: &[&str]) -> Option<usize> {
    let n = old_lines.len();
    if n == 0 || content_lines.len() < n {
        return None;
    }

    let first = old_lines[0].trim();
    let last = old_lines[n - 1].trim();

    let mut best_start = None;
    let mut best_score = 0.6_f64; // minimum threshold

    for i in 0..=content_lines.len().saturating_sub(n) {
        let end = i + n - 1;
        if end >= content_lines.len() {
            break;
        }

        let first_sim = line_similarity(content_lines[i], first);
        let last_sim = line_similarity(content_lines[end], last);

        if first_sim < 0.6 || last_sim < 0.6 {
            continue;
        }

        // Score interior lines
        let interior_score: f64 = if n > 2 {
            old_lines[1..n - 1]
                .iter()
                .zip(content_lines[i + 1..end].iter())
                .map(|(o, c)| line_similarity(c, o))
                .sum::<f64>()
                / (n - 2) as f64
        } else {
            1.0
        };

        let score = (first_sim + last_sim + interior_score) / 3.0;
        if score > best_score {
            best_score = score;
            best_start = Some(i);
        }
    }

    best_start
}

// ---------------------------------------------------------------------------
// wiki: link parsing
// ---------------------------------------------------------------------------

/// Extract all `wiki:slug` references from markdown content.
pub fn extract_wiki_links(content: &str) -> Vec<String> {
    let mut slugs = Vec::new();
    let mut chars = content.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        // Look for ](wiki:
        if c == ']'
            && let Some((_, '(')) = chars.peek().copied()
        {
            chars.next();
            let rest = &content[i + 2..];
            if let Some(end) = rest.find(')') {
                let href = &rest[..end];
                if let Some(slug) = href.strip_prefix("wiki:") {
                    slugs.push(slug.to_string());
                }
            }
        }
    }
    slugs
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WikiStore {
    pool: SqlitePool,
}

impl WikiStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Create a new wiki page (version 1).
    pub async fn create(&self, input: CreateWikiPageInput) -> Result<WikiPage> {
        let id = lower_hex_id();
        let slug = slugify(&input.title);
        let related_json =
            serde_json::to_string(&input.related).context("failed to serialize related slugs")?;

        // Check slug uniqueness; append suffix if needed
        let slug = self.unique_slug(&slug).await?;

        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to begin transaction")?;

        sqlx::query(
            r#"INSERT INTO wiki_pages
               (id, slug, title, page_type, content, related, created_by, updated_by, version, archived)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, 0)"#,
        )
        .bind(&id)
        .bind(&slug)
        .bind(&input.title)
        .bind(input.page_type.as_str())
        .bind(&input.content)
        .bind(&related_json)
        .bind(&input.author_id)
        .bind(&input.author_id)
        .execute(&mut *tx)
        .await
        .context("failed to insert wiki page")?;

        // Write version 1
        let version_id = lower_hex_id();
        sqlx::query(
            r#"INSERT INTO wiki_page_versions (id, page_id, version, content, edit_summary, author_type, author_id)
               VALUES (?, ?, 1, ?, ?, ?, ?)"#,
        )
        .bind(&version_id)
        .bind(&id)
        .bind(&input.content)
        .bind(&input.edit_summary)
        .bind(&input.author_type)
        .bind(&input.author_id)
        .execute(&mut *tx)
        .await
        .context("failed to insert wiki page version")?;

        tx.commit()
            .await
            .context("failed to commit wiki page creation")?;

        self.load_by_id(&id)
            .await?
            .context("page not found after insert")
            .map_err(crate::error::Error::from)
    }

    /// Apply a tolerant edit to an existing page, creating a new version.
    pub async fn edit(&self, input: EditWikiPageInput) -> Result<WikiPage> {
        let page = self
            .load_by_slug(&input.slug)
            .await?
            .ok_or_else(|| WikiError::NotFound {
                slug: input.slug.clone(),
            })?;

        let new_content = tolerant_replace(
            &page.content,
            &input.old_string,
            &input.new_string,
            input.replace_all,
        )
        .map_err(|e| WikiError::EditFailed(e.to_string()))?;

        let new_version = page.version + 1;

        sqlx::query(
            r#"UPDATE wiki_pages SET content = ?, version = ?, updated_by = ?,
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
               WHERE id = ?"#,
        )
        .bind(&new_content)
        .bind(new_version)
        .bind(&input.author_id)
        .bind(&page.id)
        .execute(&self.pool)
        .await
        .context("failed to update wiki page")?;

        let version_id = lower_hex_id();
        sqlx::query(
            r#"INSERT INTO wiki_page_versions (id, page_id, version, content, edit_summary, author_type, author_id)
               VALUES (?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&version_id)
        .bind(&page.id)
        .bind(new_version)
        .bind(&new_content)
        .bind(&input.edit_summary)
        .bind(&input.author_type)
        .bind(&input.author_id)
        .execute(&self.pool)
        .await
        .context("failed to insert wiki page version")?;

        self.load_by_id(&page.id)
            .await?
            .ok_or_else(|| WikiError::Other(anyhow::anyhow!("page not found after edit")))
            .map_err(crate::error::Error::from)
    }

    /// Read a page, optionally at a specific version.
    pub async fn read(&self, slug: &str, version: Option<i64>) -> Result<Option<WikiPage>> {
        if let Some(v) = version {
            // Return page with historical content
            let page = self.load_by_slug(slug).await?;
            let Some(mut page) = page else {
                return Ok(None);
            };
            let historical: Option<String> = sqlx::query_scalar(
                "SELECT content FROM wiki_page_versions WHERE page_id = ? AND version = ?",
            )
            .bind(&page.id)
            .bind(v)
            .fetch_optional(&self.pool)
            .await
            .context("failed to fetch historical version")?;

            if let Some(content) = historical {
                page.content = content;
                page.version = v;
            } else {
                return Ok(None);
            }
            Ok(Some(page))
        } else {
            self.load_by_slug(slug).await
        }
    }

    /// List all pages, optionally filtered by type. Excludes archived pages.
    pub async fn list(&self, page_type: Option<WikiPageType>) -> Result<Vec<WikiPageSummary>> {
        let rows = if let Some(pt) = page_type {
            sqlx::query(
                r#"SELECT id, slug, title, page_type, version, updated_at, updated_by
                   FROM wiki_pages WHERE archived = 0 AND page_type = ?
                   ORDER BY updated_at DESC"#,
            )
            .bind(pt.as_str())
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                r#"SELECT id, slug, title, page_type, version, updated_at, updated_by
                   FROM wiki_pages WHERE archived = 0
                   ORDER BY updated_at DESC"#,
            )
            .fetch_all(&self.pool)
            .await
        }
        .context("failed to list wiki pages")?;

        rows.into_iter()
            .map(|row| {
                Ok(WikiPageSummary {
                    id: row.try_get("id")?,
                    slug: row.try_get("slug")?,
                    title: row.try_get("title")?,
                    page_type: row.try_get("page_type")?,
                    version: row.try_get("version")?,
                    updated_at: row.try_get("updated_at")?,
                    updated_by: row.try_get("updated_by")?,
                })
            })
            .collect()
    }

    /// Full-text search across title and content.
    pub async fn search(
        &self,
        query: &str,
        page_type: Option<WikiPageType>,
    ) -> Result<Vec<WikiPageSummary>> {
        let fts_query = sanitize_fts_query(query);
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let rows = if let Some(pt) = page_type {
            sqlx::query(
                r#"SELECT p.id, p.slug, p.title, p.page_type, p.version, p.updated_at, p.updated_by
                   FROM wiki_pages_fts f
                   JOIN wiki_pages p ON p.rowid = f.rowid
                   WHERE f.wiki_pages_fts MATCH ? AND p.archived = 0 AND p.page_type = ?
                   ORDER BY rank"#,
            )
            .bind(&fts_query)
            .bind(pt.as_str())
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                r#"SELECT p.id, p.slug, p.title, p.page_type, p.version, p.updated_at, p.updated_by
                   FROM wiki_pages_fts f
                   JOIN wiki_pages p ON p.rowid = f.rowid
                   WHERE f.wiki_pages_fts MATCH ? AND p.archived = 0
                   ORDER BY rank"#,
            )
            .bind(&fts_query)
            .fetch_all(&self.pool)
            .await
        }
        .context("failed to search wiki pages")?;

        rows.into_iter()
            .map(|row| {
                Ok(WikiPageSummary {
                    id: row.try_get("id")?,
                    slug: row.try_get("slug")?,
                    title: row.try_get("title")?,
                    page_type: row.try_get("page_type")?,
                    version: row.try_get("version")?,
                    updated_at: row.try_get("updated_at")?,
                    updated_by: row.try_get("updated_by")?,
                })
            })
            .collect()
    }

    /// List version history for a page.
    pub async fn history(&self, slug: &str, limit: i64) -> Result<Vec<WikiPageVersion>> {
        let page = self.load_by_slug(slug).await?;
        let Some(page) = page else { return Ok(vec![]) };

        let rows = sqlx::query(
            r#"SELECT id, page_id, version, content, edit_summary, author_type, author_id, created_at
               FROM wiki_page_versions WHERE page_id = ? ORDER BY version DESC LIMIT ?"#,
        )
        .bind(&page.id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch wiki page history")?;

        rows.into_iter()
            .map(|row| {
                Ok(WikiPageVersion {
                    id: row.try_get("id")?,
                    page_id: row.try_get("page_id")?,
                    version: row.try_get("version")?,
                    content: row.try_get("content")?,
                    edit_summary: row.try_get("edit_summary")?,
                    author_type: row.try_get("author_type")?,
                    author_id: row.try_get("author_id")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Restore a page to a historical version (creates a new version with old content).
    pub async fn restore(
        &self,
        slug: &str,
        version: i64,
        author_type: &str,
        author_id: &str,
    ) -> Result<WikiPage> {
        let content = self
            .read(slug, Some(version))
            .await?
            .ok_or_else(|| WikiError::VersionNotFound {
                slug: slug.to_string(),
                version,
            })?
            .content;

        let page = self
            .load_by_slug(slug)
            .await?
            .ok_or_else(|| WikiError::NotFound {
                slug: slug.to_string(),
            })?;

        let edit_summary = format!("Restored to version {version}");
        let restored_content = content;
        let new_version = page.version + 1;

        sqlx::query(
            r#"UPDATE wiki_pages SET content = ?, version = ?, updated_by = ?,
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
               WHERE id = ?"#,
        )
        .bind(&restored_content)
        .bind(new_version)
        .bind(author_id)
        .bind(&page.id)
        .execute(&self.pool)
        .await
        .context("failed to restore wiki page")?;

        let version_id = lower_hex_id();
        sqlx::query(
            r#"INSERT INTO wiki_page_versions (id, page_id, version, content, edit_summary, author_type, author_id)
               VALUES (?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&version_id)
        .bind(&page.id)
        .bind(new_version)
        .bind(&restored_content)
        .bind(&edit_summary)
        .bind(author_type)
        .bind(author_id)
        .execute(&self.pool)
        .await
        .context("failed to insert restored version")?;

        self.load_by_id(&page.id)
            .await?
            .ok_or_else(|| WikiError::Other(anyhow::anyhow!("page not found after restore")))
            .map_err(crate::error::Error::from)
    }

    /// Archive a page (soft delete).
    pub async fn archive(&self, slug: &str) -> Result<()> {
        sqlx::query("UPDATE wiki_pages SET archived = 1 WHERE slug = ?")
            .bind(slug)
            .execute(&self.pool)
            .await
            .context("failed to archive wiki page")?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    async fn load_by_id(&self, id: &str) -> Result<Option<WikiPage>> {
        let row = sqlx::query(
            "SELECT id, slug, title, page_type, content, related, created_by, updated_by, version, archived, created_at, updated_at FROM wiki_pages WHERE id = ?"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to load wiki page by id")?;
        row.map(parse_wiki_page).transpose()
    }

    pub async fn load_by_slug(&self, slug: &str) -> Result<Option<WikiPage>> {
        let row = sqlx::query(
            "SELECT id, slug, title, page_type, content, related, created_by, updated_by, version, archived, created_at, updated_at FROM wiki_pages WHERE slug = ?"
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .context("failed to load wiki page by slug")?;
        row.map(parse_wiki_page).transpose()
    }

    async fn unique_slug(&self, base: &str) -> Result<String> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wiki_pages WHERE slug = ?)")
                .bind(base)
                .fetch_one(&self.pool)
                .await
                .context("failed to check slug uniqueness")?;

        if !exists {
            return Ok(base.to_string());
        }

        for i in 2..=999 {
            let candidate = format!("{base}-{i}");
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wiki_pages WHERE slug = ?)")
                    .bind(&candidate)
                    .fetch_one(&self.pool)
                    .await
                    .context("failed to check slug uniqueness")?;
            if !exists {
                return Ok(candidate);
            }
        }

        Err(anyhow::anyhow!("failed to find unique slug for '{base}'").into())
    }
}

fn parse_wiki_page(row: sqlx::sqlite::SqliteRow) -> Result<WikiPage> {
    let related_json: String = row.try_get("related")?;
    let related: Vec<String> = serde_json::from_str(&related_json).unwrap_or_default();
    let archived: i64 = row.try_get("archived")?;
    Ok(WikiPage {
        id: row.try_get("id")?,
        slug: row.try_get("slug")?,
        title: row.try_get("title")?,
        page_type: row.try_get("page_type")?,
        content: row.try_get("content")?,
        related,
        created_by: row.try_get("created_by")?,
        updated_by: row.try_get("updated_by")?,
        version: row.try_get("version")?,
        archived: archived != 0,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn lower_hex_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Escape user input for use as an FTS5 MATCH query.
///
/// FTS5 treats characters like `"`, `*`, `:`, `(`, `)` and bareword operators
/// (`AND`, `OR`, `NOT`, `NEAR`) as syntax. We strip the input down to
/// alphanumeric tokens (plus `_` and `-`) and append `*` to the last token for
/// prefix matching. Returns an empty string when no usable tokens remain, in
/// which case the caller should short-circuit instead of issuing the query.
fn sanitize_fts_query(input: &str) -> String {
    let tokens: Vec<String> = input
        .split_whitespace()
        .map(|token| {
            token
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>()
        })
        .filter(|token| !token.is_empty())
        .collect();

    if tokens.is_empty() {
        return String::new();
    }

    let mut parts: Vec<String> = tokens
        .iter()
        .take(tokens.len() - 1)
        .map(|t| format!("\"{t}\""))
        .collect();
    let last = tokens.last().expect("tokens is non-empty");
    parts.push(format!("\"{last}\"*"));
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::sanitize_fts_query;

    #[test]
    fn sanitizes_fts_operators() {
        assert_eq!(sanitize_fts_query("hello"), "\"hello\"*");
        assert_eq!(sanitize_fts_query("hello world"), "\"hello\" \"world\"*");
        assert_eq!(sanitize_fts_query(""), "");
        assert_eq!(sanitize_fts_query("   "), "");
        // Special characters are stripped so they cannot break the query.
        assert_eq!(
            sanitize_fts_query("foo AND bar"),
            "\"foo\" \"AND\" \"bar\"*"
        );
        assert_eq!(sanitize_fts_query("col:val"), "\"colval\"*");
        assert_eq!(sanitize_fts_query("\"phrase\""), "\"phrase\"*");
        assert_eq!(sanitize_fts_query("a*b"), "\"ab\"*");
    }
}
