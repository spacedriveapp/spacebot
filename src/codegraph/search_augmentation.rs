//! BM25 fast-path search augmentation.
//!
//! The full hybrid search in [`super::search::hybrid_search`] fuses
//! FTS + vector similarity and has cold-start latency of several
//! hundred milliseconds because it spins up the embedding model on
//! first call. For interactive uses — IDE completion, palette
//! suggestions, call-chain spelunking — that's too slow.
//!
//! The augmentation layer here is BM25-only: FTS hit ⇒ node-detail
//! enrichment (qname, label, line, file). It aims for <500ms cold
//! and <200ms warm on a 5k-file project, matching GitNexus's
//! augmentation-engine targets. A timeout is applied so callers can
//! safely fall back to the slower hybrid path when the FTS
//! extension isn't available.
//!
//! Callers feed the result directly into graph exploration: the
//! returned [`AugmentationHit`] carries enough context to be
//! expanded into caller / callee chains without an additional
//! lookup round-trip.

use std::time::Duration;

use anyhow::Result;
use tokio::time::timeout;

use super::db::SharedCodeGraphDb;

/// Cap on augmentation query latency. Cold-path queries may briefly
/// exceed this if the FTS extension hasn't finished loading; the
/// timeout is the caller's signal to fall back to the slower hybrid
/// path rather than block the UI.
pub const AUGMENTATION_TIMEOUT: Duration = Duration::from_millis(500);

/// Maximum hits returned per augmentation query. Enough to feed a
/// palette / completion list; less than the hybrid-search cap to
/// keep rendering cheap.
pub const AUGMENTATION_LIMIT: usize = 25;

/// One row returned by [`augment`]. Fields are minimal — callers
/// that want full node details resolve by qname against the hybrid
/// API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AugmentationHit {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub source_file: String,
    pub line_start: i32,
}

/// Run a fast-path BM25 search against the project's FTS indexes.
///
/// Returns `Ok(Vec::new())` when the FTS extension is unavailable or
/// the query times out — the caller is expected to silently fall
/// back to the full hybrid path in those cases. This mirrors
/// GitNexus's "graceful failure" semantics.
pub async fn augment(
    db: &SharedCodeGraphDb,
    project_id: &str,
    query: &str,
) -> Result<Vec<AugmentationHit>> {
    let work = do_augment(db, project_id, query);
    match timeout(AUGMENTATION_TIMEOUT, work).await {
        Ok(Ok(hits)) => Ok(hits),
        Ok(Err(err)) => {
            tracing::debug!(%err, "augmentation BM25 path failed; caller should fall back");
            Ok(Vec::new())
        }
        Err(_) => {
            tracing::debug!(query, "augmentation timed out; caller should fall back");
            Ok(Vec::new())
        }
    }
}

async fn do_augment(
    db: &SharedCodeGraphDb,
    project_id: &str,
    query: &str,
) -> Result<Vec<AugmentationHit>> {
    if !db.load_fts_extension().await? {
        return Ok(Vec::new());
    }
    let pid = project_id.replace('\\', "\\\\").replace('\'', "\\'");
    let q = query.replace('\\', "\\\\").replace('\'', "\\'");

    // Pick a small set of the most-searched labels; the full hybrid
    // search queries every FTS-indexed table, but for augmentation
    // we trade coverage for latency.
    let labels = [
        ("Function", "function_fts"),
        ("Method", "method_fts"),
        ("Class", "class_fts"),
        ("Interface", "interface_fts"),
        ("Struct", "struct_fts"),
        ("Route", "route_fts"),
        ("File", "file_fts"),
    ];

    let mut hits: Vec<AugmentationHit> = Vec::new();
    for (label, index) in labels {
        let cypher = format!(
            "CALL QUERY_FTS_INDEX('{label}', '{index}', '{q}') \
             WITH node AS n, score \
             WHERE n.project_id = '{pid}' \
             RETURN n.qualified_name, n.name, n.source_file, n.line_start, score \
             ORDER BY score DESC \
             LIMIT {limit}",
            limit = AUGMENTATION_LIMIT
        );
        let rows = match db.query(&cypher).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in rows {
            let qname = match row.first() {
                Some(lbug::Value::String(s)) => s.clone(),
                _ => continue,
            };
            let name = match row.get(1) {
                Some(lbug::Value::String(s)) => s.clone(),
                _ => continue,
            };
            let source_file = match row.get(2) {
                Some(lbug::Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            let line_start = match row.get(3) {
                Some(lbug::Value::Int32(n)) => *n,
                Some(lbug::Value::Int64(n)) => *n as i32,
                _ => 0,
            };
            hits.push(AugmentationHit {
                qualified_name: qname,
                name,
                label: label.to_string(),
                source_file,
                line_start,
            });
        }
    }

    // Truncate to the advertised limit — we picked per-label, so
    // multi-label hits can exceed the cap.
    hits.truncate(AUGMENTATION_LIMIT);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Integration-style tests against a real LadybugDB require an
    // initialised DB + indexed project, which is outside the scope
    // of this unit test pass. The runtime behavior is covered by
    // the existing `tests/parity.rs` suite's search assertions.

    #[test]
    fn hit_fields_round_trip() {
        let h = AugmentationHit {
            qualified_name: "crate::foo".to_string(),
            name: "foo".to_string(),
            label: "Function".to_string(),
            source_file: "lib.rs".to_string(),
            line_start: 10,
        };
        assert_eq!(h.qualified_name, "crate::foo");
    }
}
