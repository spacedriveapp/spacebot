//! Tree-sitter AST cache.
//!
//! Parsing with tree-sitter isn't free: for a 10 kLOC file the parse
//! plus byte-to-AST conversion can take tens of milliseconds, and the
//! migration to query-based extraction in Phase 2 Part 2 will pass the
//! same tree through multiple extractors (symbols, calls, fields,
//! exports, …). Without a cache each pass would re-parse — doubling
//! or tripling the cost on every large project.
//!
//! The cache is a thread-safe LRU keyed by `(path, content_hash)`. A
//! cache miss means the caller re-parses, stores, and moves on; a hit
//! hands back an `Arc<CachedAst>` that can be read in parallel by any
//! number of extractors.
//!
//! Entries contain both the parsed `tree_sitter::Tree` **and** the
//! source string it was parsed from, because every query / walker
//! needs the source bytes to resolve node ranges back into text.
//! Keeping source and tree paired prevents a common footgun where a
//! cached tree is read against a newer-on-disk source and mis-reports
//! node text.

use std::sync::Arc;

use moka::sync::Cache;

/// Hard cap on the number of distinct `(path, content_hash)` entries
/// the cache keeps. Matches GitNexus's 50-tree LRU exactly — large
/// enough to hold every file in a typical module during a multi-pass
/// extraction, small enough to bound worst-case memory when a
/// pathological project opens hundreds of files in a burst.
const MAX_ENTRIES: u64 = 50;

/// A parsed source file. Both fields share a lifetime via [`Arc`]
/// because downstream code routinely needs to walk the tree and slice
/// into the source simultaneously — separating them would force an
/// extra reference-counted wrapper at every call site.
#[derive(Debug)]
pub struct CachedAst {
    /// The parsed syntax tree. Tree-sitter trees are append-only and
    /// safe to share across threads.
    pub tree: tree_sitter::Tree,
    /// The exact source bytes the tree was parsed from. Stored as
    /// `Arc<str>` to avoid re-allocating it per clone.
    pub source: Arc<str>,
}

/// Lightweight, thread-safe AST cache.
///
/// Cloning an `AstCache` gives every caller a view of the same
/// underlying storage — the type wraps an `Arc<Cache<_,_>>` so sharing
/// across pipeline phases is cheap.
#[derive(Clone)]
pub struct AstCache {
    inner: Cache<CacheKey, Arc<CachedAst>>,
}

/// Cache key. A pair (path, hash) rather than path alone so a file
/// that's been edited since its last insertion is treated as a new
/// entry instead of returning stale content.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    path: String,
    content_hash: u64,
}

impl CacheKey {
    /// Derive a key from a path string and a content slice. The hash
    /// is deterministic within a single process run; it is **not**
    /// intended to be stable across processes — the cache is an
    /// in-memory optimization, never a persistence layer.
    pub fn new(path: &str, content: &[u8]) -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        // `RandomState::new().build_hasher()` gives a SipHash instance
        // that's fast, collision-resistant enough for cache keying,
        // and doesn't require pulling in a third-party hash crate.
        let mut h = RandomState::new().build_hasher();
        h.write(content);
        Self {
            path: path.to_string(),
            content_hash: h.finish(),
        }
    }
}

impl Default for AstCache {
    fn default() -> Self {
        Self::new()
    }
}

impl AstCache {
    /// Create an empty cache with the default capacity.
    pub fn new() -> Self {
        Self {
            inner: Cache::builder().max_capacity(MAX_ENTRIES).build(),
        }
    }

    /// Create a cache with a custom capacity. Primarily for tests
    /// — production code should stick to [`Self::new`] so every
    /// subsystem that consults the cache agrees on its size.
    pub fn with_capacity(max_entries: u64) -> Self {
        Self {
            inner: Cache::builder().max_capacity(max_entries).build(),
        }
    }

    /// Look up an entry. Returns `None` on miss; callers re-parse and
    /// [`Self::insert`].
    pub fn get(&self, key: &CacheKey) -> Option<Arc<CachedAst>> {
        self.inner.get(key)
    }

    /// Store an entry. Drops the previous value under the same key if
    /// any — the shared `Arc` keeps existing readers alive until they
    /// finish.
    pub fn insert(&self, key: CacheKey, ast: Arc<CachedAst>) {
        self.inner.insert(key, ast);
    }

    /// Convenience: look up `(path, content)`, running `parse` on
    /// miss. `parse` must return `Ok(Tree)` on success; failures are
    /// propagated unchanged so parse errors don't get silently
    /// converted into a missing-entry signal.
    pub fn get_or_parse<F>(
        &self,
        path: &str,
        content: &str,
        parse: F,
    ) -> anyhow::Result<Arc<CachedAst>>
    where
        F: FnOnce(&str) -> anyhow::Result<tree_sitter::Tree>,
    {
        let key = CacheKey::new(path, content.as_bytes());
        if let Some(hit) = self.get(&key) {
            return Ok(hit);
        }
        let tree = parse(content)?;
        let cached = Arc::new(CachedAst {
            tree,
            source: Arc::from(content),
        });
        self.insert(key, Arc::clone(&cached));
        Ok(cached)
    }

    /// Current entry count (approximate; moka runs eviction lazily).
    pub fn len(&self) -> u64 {
        self.inner.entry_count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    /// Build a Python parser. Python is the smallest tree-sitter
    /// grammar in our deps tree, which keeps test compile time low.
    fn python_parser() -> Parser {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("python grammar");
        parser
    }

    fn parse_python(source: &str) -> anyhow::Result<tree_sitter::Tree> {
        python_parser()
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("parse failed"))
    }

    #[test]
    fn key_changes_when_content_changes() {
        let a = CacheKey::new("foo.py", b"x = 1");
        let b = CacheKey::new("foo.py", b"x = 2");
        assert_ne!(a, b);
    }

    #[test]
    fn key_stable_for_identical_inputs() {
        let a = CacheKey::new("foo.py", b"x = 1");
        let b = CacheKey::new("foo.py", b"x = 1");
        assert_eq!(a, b);
    }

    #[test]
    fn get_miss_returns_none() {
        let cache = AstCache::new();
        assert!(cache.get(&CacheKey::new("x.py", b"")).is_none());
    }

    #[test]
    fn insert_then_get_round_trips() {
        let cache = AstCache::new();
        let tree = parse_python("x = 1").unwrap();
        let key = CacheKey::new("x.py", b"x = 1");
        cache.insert(
            key.clone(),
            Arc::new(CachedAst {
                tree,
                source: Arc::from("x = 1"),
            }),
        );
        let hit = cache.get(&key).expect("insert then get");
        assert_eq!(&*hit.source, "x = 1");
    }

    #[test]
    fn get_or_parse_parses_once() {
        let cache = AstCache::new();
        let calls = std::cell::Cell::new(0u32);
        let parse = |s: &str| {
            calls.set(calls.get() + 1);
            parse_python(s)
        };
        // First call: miss → parse once.
        let a = cache
            .get_or_parse("f.py", "x = 1", parse)
            .expect("first parse");
        assert_eq!(calls.get(), 1);
        // Second call with same content: hit, no re-parse.
        let calls2 = std::cell::Cell::new(0u32);
        let parse2 = |s: &str| {
            calls2.set(calls2.get() + 1);
            parse_python(s)
        };
        let b = cache
            .get_or_parse("f.py", "x = 1", parse2)
            .expect("cache hit");
        assert_eq!(calls2.get(), 0, "cached call should not re-parse");
        assert!(Arc::ptr_eq(&a, &b), "same Arc returned on hit");
    }

    #[test]
    fn content_edit_triggers_re_parse() {
        let cache = AstCache::new();
        let _ = cache
            .get_or_parse("f.py", "x = 1", parse_python)
            .unwrap();
        let calls = std::cell::Cell::new(0u32);
        let _ = cache
            .get_or_parse("f.py", "x = 2", |s| {
                calls.set(calls.get() + 1);
                parse_python(s)
            })
            .unwrap();
        assert_eq!(calls.get(), 1, "edited content must re-parse");
    }

    #[test]
    fn capacity_bounds_size() {
        let cache = AstCache::with_capacity(2);
        for i in 0..5 {
            let src = format!("x = {i}");
            cache
                .get_or_parse(&format!("f{i}.py"), &src, parse_python)
                .unwrap();
        }
        // Moka eviction is asynchronous; run_pending_tasks forces it
        // to catch up synchronously so the assertion is stable.
        cache.inner.run_pending_tasks();
        assert!(
            cache.len() <= 2,
            "capacity-bounded cache kept {} entries",
            cache.len()
        );
    }
}
