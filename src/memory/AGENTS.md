# memory/ — Implementation Notes

Six files. Two databases. One search pipeline.

## Files

- `types.rs` — `Memory`, `Association`, `MemoryType`, `RelationType`. Pure data, no I/O.
- `store.rs` (769 lines) — `MemoryStore`: all SQLite ops. CRUD + graph edge management (`find_associations`, `add_association`). Queries live here, not in `db/`.
- `lance.rs` — `EmbeddingTable`: LanceDB table lifecycle. HNSW index for vector similarity. Tantivy for full-text search. Paired to SQLite rows via memory ID.
- `embedding.rs` — `EmbeddingModel`: local embedding generation via FastEmbed. No API calls, no network dependency.
- `search.rs` — `MemorySearch`: hybrid search pipeline. `SearchConfig`, `SearchMode`, `SearchSort`.
- `maintenance.rs` — background ops: decay, prune, merge, reindex.

## Search Pipeline

`MemorySearch` runs vector + FTS in parallel, merges via RRF, then optionally traverses graph edges.

```
query
  ├── EmbeddingModel.embed(query)  →  LanceDB HNSW  →  [(id, score), ...]  ranked list A
  └── Tantivy FTS(query)           →  [(id, score), ...]  ranked list B
        ↓
      RRF merge: score = Σ 1/(60 + rank)   (rank from each list, not raw scores)
        ↓
      top-N candidates  →  store.load_many(ids)  →  full Memory structs
        ↓
      optional graph traversal: follow edges from top results, pull associated memories
        ↓
      sort by SearchSort (Recent | Importance | MostAccessed)
```

`SearchMode` shortcuts: `Recent` skips vector search entirely. `Important` filters by importance threshold before RRF. `Typed` filters by `MemoryType` post-merge. `Hybrid` runs the full pipeline.

RRF constant is 60 (standard). Ranks are 1-indexed. Scores are not normalized before merging.

## Graph Structure

Edges live in SQLite (`store.rs`). `Association` has `source_id`, `target_id`, `relation_type`, `strength` (0.0..1.0).

`RelationType` variants: `RelatedTo`, `Updates`, `Contradicts`, `CausedBy`, `PartOf`.

Auto-association on save: similarity search runs at write time. Hits above 0.9 cosine similarity get an `Updates` edge. Lower hits get `RelatedTo`. This happens inside `store.rs`, not in the tool layer.

Graph traversal in search is breadth-first, one hop by default. Depth is configurable via `SearchConfig`.
## Maintenance

All ops in `maintenance.rs`. Run by the cortex or a scheduled worker, not inline.

- **Decay**: reduces `importance` on memories not accessed recently. Identity memories (`MemoryType::Identity`) are exempt.
- **Prune**: deletes memories below an importance floor. Checks for orphaned LanceDB embeddings after deletion.
- **Merge**: finds near-duplicate pairs (high cosine similarity + same type), collapses into one, rewires edges.
- **Reindex**: drops and rebuilds LanceDB HNSW + Tantivy indices. Needed after bulk deletes or schema changes.

Maintenance ops are fallible but non-fatal. Errors are logged, not propagated to callers.

## Invariants

- Every `Memory` row in SQLite must have a corresponding embedding in LanceDB. Orphan detection is in `maintenance.rs`.
- `store.rs` owns all SQLite queries. `lance.rs` owns all LanceDB queries. No cross-file raw SQL.
- Embedding generation is synchronous inside `EmbeddingModel`. Callers wrap in `spawn_blocking` if needed.
