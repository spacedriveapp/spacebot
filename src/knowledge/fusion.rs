//! Rank fusion for mixed knowledge retrieval results.

use crate::knowledge::types::KnowledgeHit;

use std::collections::HashMap;

const DEFAULT_RRF_K: f32 = 60.0;

#[derive(Debug, Clone)]
struct FusedHit {
    hit: KnowledgeHit,
    fused_score: f32,
}

pub fn reciprocal_rank_fuse(
    hit_lists: &[Vec<KnowledgeHit>],
    max_results: usize,
) -> Vec<KnowledgeHit> {
    let mut fused_hits = HashMap::<(String, String), FusedHit>::new();

    for hits in hit_lists {
        for (rank, hit) in hits.iter().enumerate() {
            let key = (hit.provenance.source_id.clone(), hit.id.clone());
            let rank_score = 1.0 / (DEFAULT_RRF_K + rank as f32 + 1.0);
            fused_hits
                .entry(key)
                .and_modify(|entry| {
                    entry.fused_score += rank_score;
                    if hit.score > entry.hit.score {
                        entry.hit = hit.clone();
                    }
                })
                .or_insert_with(|| FusedHit {
                    hit: hit.clone(),
                    fused_score: rank_score,
                });
        }
    }

    let mut fused_hits = fused_hits.into_values().collect::<Vec<_>>();
    fused_hits.sort_by(|left, right| {
        right
            .fused_score
            .partial_cmp(&left.fused_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right
                    .hit
                    .score
                    .partial_cmp(&left.hit.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.hit.title.cmp(&right.hit.title))
    });

    fused_hits
        .into_iter()
        .take(max_results)
        .map(|entry| entry.hit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::reciprocal_rank_fuse;
    use crate::knowledge::types::{KnowledgeHit, KnowledgeProvenance, KnowledgeSourceKind};

    fn hit(
        id: &str,
        title: &str,
        score: f32,
        source_id: &str,
        source_kind: KnowledgeSourceKind,
    ) -> KnowledgeHit {
        KnowledgeHit {
            id: id.to_string(),
            title: title.to_string(),
            snippet: format!("snippet for {title}"),
            content_type: "text/markdown".to_string(),
            score,
            provenance: KnowledgeProvenance {
                source_id: source_id.to_string(),
                source_kind,
                source_label: source_id.to_string(),
                canonical_locator: format!("{source_id}://{id}"),
            },
        }
    }

    #[test]
    fn reciprocal_rank_fuse_preserves_provenance_for_mixed_sources() {
        let fused = reciprocal_rank_fuse(
            &[
                vec![hit(
                    "memory-1",
                    "Native Memory",
                    0.91,
                    "native_memory",
                    KnowledgeSourceKind::NativeMemory,
                )],
                vec![hit(
                    "qmd-1",
                    "QMD Note",
                    0.83,
                    "qmd",
                    KnowledgeSourceKind::Qmd,
                )],
            ],
            10,
        );

        assert_eq!(fused.len(), 2);
        assert!(
            fused
                .iter()
                .any(|entry| entry.provenance.source_id == "native_memory")
        );
        assert!(
            fused
                .iter()
                .any(|entry| entry.provenance.source_id == "qmd")
        );
    }

    #[test]
    fn reciprocal_rank_fuse_keeps_distinct_hits_when_ids_contain_separator_text() {
        let fused = reciprocal_rank_fuse(
            &[
                vec![hit(
                    "alpha::beta",
                    "First",
                    0.91,
                    "qmd",
                    KnowledgeSourceKind::Qmd,
                )],
                vec![hit(
                    "beta",
                    "Second",
                    0.83,
                    "qmd::alpha",
                    KnowledgeSourceKind::Qmd,
                )],
            ],
            10,
        );

        assert_eq!(fused.len(), 2);
        assert!(fused.iter().any(|entry| entry.title == "First"));
        assert!(fused.iter().any(|entry| entry.title == "Second"));
    }
}
