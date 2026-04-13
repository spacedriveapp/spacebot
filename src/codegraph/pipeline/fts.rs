//! Full-text search index using SQLite FTS5.
//!
//! Builds a lightweight FTS5 table alongside the LadybugDB graph so
//! symbol search can use BM25 ranking without needing LadybugDB to
//! support full-text operators natively.

use std::path::Path;

use anyhow::{Context, Result};

use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;

/// Build (or rebuild) the FTS5 index for a project.
///
/// Creates `fts.sqlite` next to the `lbug/` directory and populates it
/// with every Function, Method, Class, Interface, Struct, and Trait
/// node's qualified_name, name, and source_file.
pub async fn build_fts_index(
    project_id: &str,
    db: &SharedCodeGraphDb,
) -> Result<PhaseResult> {
    let mut result = PhaseResult::default();

    // Resolve the FTS db path from the LadybugDB path:
    //   .spacebot/codegraph/<project_id>/lbug/ → .../fts.sqlite
    let fts_path = db.db_path.parent().unwrap_or(Path::new(".")).join("fts.sqlite");

    let fts_url = format!("sqlite:{}?mode=rwc", fts_path.to_string_lossy().replace('\\', "/"));
    let pool = sqlx::sqlite::SqlitePool::connect(&fts_url)
        .await
        .with_context(|| format!("opening FTS database at {}", fts_path.display()))?;

    // Drop and recreate so re-index is idempotent.
    sqlx::query("DROP TABLE IF EXISTS symbols_fts")
        .execute(&pool)
        .await?;
    sqlx::query("DROP TABLE IF EXISTS symbols_content")
        .execute(&pool)
        .await?;

    sqlx::query(
        "CREATE TABLE symbols_content (\
         id INTEGER PRIMARY KEY AUTOINCREMENT, \
         qualified_name TEXT NOT NULL, \
         name TEXT NOT NULL, \
         label TEXT NOT NULL, \
         source_file TEXT NOT NULL, \
         project_id TEXT NOT NULL)"
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE VIRTUAL TABLE symbols_fts USING fts5(\
         name, qualified_name, label, source_file, \
         content=symbols_content, content_rowid=id)"
    )
    .execute(&pool)
    .await?;

    let pid = project_id.replace('\\', "\\\\").replace('\'', "\\'");
    // Only index labels that survive the pipeline cleanup. Variable,
    // Import, Parameter, and Decorator are deleted before this runs.
    let searchable_labels = &[
        "Function", "Method", "Class", "Interface", "Struct", "Trait",
        "Enum", "TypeAlias", "Const", "Module", "Route",
    ];

    let mut total_indexed = 0u64;
    for label in searchable_labels {
        let rows = db
            .query(&format!(
                "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
                 RETURN n.qualified_name, n.name, n.source_file"
            ))
            .await?;

        for row in &rows {
            if let (
                Some(lbug::Value::String(qname)),
                Some(lbug::Value::String(name)),
                Some(lbug::Value::String(sf)),
            ) = (row.first(), row.get(1), row.get(2))
            {
                sqlx::query(
                    "INSERT INTO symbols_content (qualified_name, name, label, source_file, project_id) \
                     VALUES (?, ?, ?, ?, ?)"
                )
                .bind(qname)
                .bind(name)
                .bind(*label)
                .bind(sf)
                .bind(project_id)
                .execute(&pool)
                .await?;

                total_indexed += 1;
            }
        }
    }

    // Rebuild the FTS index from the content table.
    sqlx::query("INSERT INTO symbols_fts(symbols_fts) VALUES('rebuild')")
        .execute(&pool)
        .await?;

    pool.close().await;

    tracing::info!(
        project_id = %project_id,
        indexed = total_indexed,
        path = %fts_path.display(),
        "FTS index built"
    );

    result.nodes_created = total_indexed;
    Ok(result)
}
