//! File-level topological parse ordering.
//!
//! Given a set of files and the imports declared by each, this module
//! produces a "levels" ordering: files with zero unresolved imports
//! land in level 0, files that import level-0 files land in level 1,
//! and so on. Files that participate in an import cycle fall into the
//! final "unordered" bucket — cycles are rare and we don't try to
//! break them; the call-resolution phase downstream is tier-scored
//! specifically because cycles exist.
//!
//! The Phase 4 call resolver consumes this ordering so type
//! environments for dependencies are populated before dependents need
//! them — calling `user_service.create(...)` in `handler.py` resolves
//! the receiver type by looking at `user_service.py`, which must have
//! been parsed first.
//!
//! Today the walker hands files to the parsing phase in directory
//! order. This module slots in *after* the walker so parsing can
//! follow the dependency topology instead. A cycle-tolerant Kahn's
//! scan runs in O(V + E) and never allocates per-file state beyond
//! `HashMap` buckets — cheap even on 50k-file repos.

use std::collections::{HashMap, HashSet};

/// An ordered partition of files into dependency levels.
///
/// - `levels[0]` holds files with no incoming dependencies — safe to
///   parse first.
/// - `levels[k]` for `k > 0` holds files whose dependencies were all
///   covered by earlier levels.
/// - `cycles` holds files that couldn't be placed because they
///   participate in at least one import cycle.
///
/// Consumers parse level-by-level (optionally in parallel within a
/// level) and then parse `cycles` last in any order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParseOrder {
    pub levels: Vec<Vec<String>>,
    pub cycles: Vec<String>,
}

impl ParseOrder {
    /// True when every file landed in some level — no cycles were
    /// detected.
    pub fn is_acyclic(&self) -> bool {
        self.cycles.is_empty()
    }

    /// Flatten into a single parse sequence. Levels appear in order,
    /// with cycle members appended at the end. Caller loses parallel-
    /// within-level information; use [`Self::levels`] directly when
    /// that matters.
    pub fn flatten(&self) -> Vec<String> {
        let cap: usize = self.levels.iter().map(Vec::len).sum::<usize>() + self.cycles.len();
        let mut out = Vec::with_capacity(cap);
        for level in &self.levels {
            out.extend(level.iter().cloned());
        }
        out.extend(self.cycles.iter().cloned());
        out
    }

    /// Total files ordered, including cycle members.
    pub fn total(&self) -> usize {
        self.levels.iter().map(Vec::len).sum::<usize>() + self.cycles.len()
    }
}

/// Sort `files` into dependency levels using their declared imports.
///
/// `imports` is a map from file path → set of file paths that file
/// imports. Imports pointing outside the set (external packages,
/// stdlib) are ignored silently — we only care about the intra-
/// project edges that create ordering constraints.
///
/// Files appear in each level in lexicographic order so two runs on
/// the same input produce byte-identical output (important for
/// parity tests and deterministic builds).
pub fn topological_levels(
    files: &[String],
    imports: &HashMap<String, HashSet<String>>,
) -> ParseOrder {
    // Only edges that live fully inside `files` count — we need a
    // `HashSet` of the file list first for O(1) membership checks.
    let file_set: HashSet<&str> = files.iter().map(String::as_str).collect();

    // Build the filtered dependency graph + in-degree counts.
    // `deps[a]` lists files that `a` depends on (i.e. `a` imports
    // each of them). In-degree here means "how many files in the
    // pending set does this file depend on".
    let mut deps: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    for f in &file_set {
        in_degree.insert(f, 0);
        deps.insert(f, Vec::new());
    }
    for (src, targets) in imports {
        if !file_set.contains(src.as_str()) {
            continue;
        }
        let src_ref = *file_set.get(src.as_str()).expect("just checked");
        for tgt in targets {
            if let Some(&tgt_ref) = file_set.get(tgt.as_str()) {
                if tgt_ref == src_ref {
                    continue; // self-imports don't create ordering
                }
                deps.get_mut(src_ref).expect("init").push(tgt_ref);
                *in_degree.get_mut(src_ref).expect("init") += 1;
            }
        }
    }

    // BFS-style peeling: each round collects every file whose deps
    // are already satisfied (in-degree drops to 0), emits them as a
    // level, and decrements in-degree for any file that depends on
    // the freshly-placed ones.
    //
    // `reverse_deps[t]` gives the list of files that import `t` — so
    // when `t` lands in a level we can cheaply find everyone whose
    // in-degree should be decremented. Without this we'd need to
    // re-scan every pending file on every round.
    let mut reverse_deps: HashMap<&str, Vec<&str>> = HashMap::new();
    for (src, targets) in &deps {
        for &tgt in targets {
            reverse_deps.entry(tgt).or_default().push(src);
        }
    }

    let mut levels: Vec<Vec<String>> = Vec::new();
    let mut placed: HashSet<&str> = HashSet::new();

    loop {
        let mut frontier: Vec<&str> = in_degree
            .iter()
            .filter_map(|(&f, &d)| {
                if d == 0 && !placed.contains(f) {
                    Some(f)
                } else {
                    None
                }
            })
            .collect();
        if frontier.is_empty() {
            break;
        }
        frontier.sort();

        for f in &frontier {
            placed.insert(*f);
            if let Some(parents) = reverse_deps.get(*f) {
                for &parent in parents {
                    if let Some(d) = in_degree.get_mut(parent) {
                        *d = d.saturating_sub(1);
                    }
                }
            }
        }
        levels.push(frontier.iter().map(|s| s.to_string()).collect());
    }

    // Anything unplaced sits in at least one cycle.
    let mut cycles: Vec<String> = file_set
        .iter()
        .filter(|f| !placed.contains(**f))
        .map(|s| s.to_string())
        .collect();
    cycles.sort();

    ParseOrder { levels, cycles }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_imports(pairs: &[(&str, &[&str])]) -> HashMap<String, HashSet<String>> {
        pairs
            .iter()
            .map(|(src, tgts)| {
                (
                    src.to_string(),
                    tgts.iter().map(|s| s.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn independent_files_land_in_level_zero() {
        let files = vec!["a.rs".into(), "b.rs".into(), "c.rs".into()];
        let order = topological_levels(&files, &HashMap::new());
        assert_eq!(order.levels.len(), 1);
        assert_eq!(order.levels[0], vec!["a.rs", "b.rs", "c.rs"]);
        assert!(order.is_acyclic());
    }

    #[test]
    fn chain_produces_levels_in_dep_order() {
        // a imports b imports c
        let files = vec!["a.rs".into(), "b.rs".into(), "c.rs".into()];
        let imports = mk_imports(&[("a.rs", &["b.rs"]), ("b.rs", &["c.rs"])]);
        let order = topological_levels(&files, &imports);
        // Level 0 = c (nobody it depends on); level 1 = b; level 2 = a
        assert_eq!(order.levels[0], vec!["c.rs"]);
        assert_eq!(order.levels[1], vec!["b.rs"]);
        assert_eq!(order.levels[2], vec!["a.rs"]);
        assert!(order.is_acyclic());
    }

    #[test]
    fn diamond_levels_are_correct() {
        // a -> b, a -> c, b -> d, c -> d
        let files = vec!["a.rs".into(), "b.rs".into(), "c.rs".into(), "d.rs".into()];
        let imports = mk_imports(&[
            ("a.rs", &["b.rs", "c.rs"]),
            ("b.rs", &["d.rs"]),
            ("c.rs", &["d.rs"]),
        ]);
        let order = topological_levels(&files, &imports);
        assert_eq!(order.levels[0], vec!["d.rs"]);
        assert_eq!(order.levels[1], vec!["b.rs", "c.rs"]);
        assert_eq!(order.levels[2], vec!["a.rs"]);
    }

    #[test]
    fn cycle_members_isolated() {
        // a -> b -> a (cycle), c is independent
        let files = vec!["a.rs".into(), "b.rs".into(), "c.rs".into()];
        let imports = mk_imports(&[("a.rs", &["b.rs"]), ("b.rs", &["a.rs"])]);
        let order = topological_levels(&files, &imports);
        // c is independent and lands at level 0; a, b are cycle
        // members.
        assert_eq!(order.levels[0], vec!["c.rs"]);
        assert_eq!(order.cycles, vec!["a.rs", "b.rs"]);
        assert!(!order.is_acyclic());
    }

    #[test]
    fn external_imports_ignored() {
        let files = vec!["a.rs".into(), "b.rs".into()];
        // a imports b + an external path that isn't in `files`
        let imports = mk_imports(&[("a.rs", &["b.rs", "std::collections"])]);
        let order = topological_levels(&files, &imports);
        assert_eq!(order.levels[0], vec!["b.rs"]);
        assert_eq!(order.levels[1], vec!["a.rs"]);
    }

    #[test]
    fn self_import_treated_as_no_constraint() {
        let files = vec!["a.rs".into()];
        let imports = mk_imports(&[("a.rs", &["a.rs"])]);
        let order = topological_levels(&files, &imports);
        assert_eq!(order.levels[0], vec!["a.rs"]);
        assert!(order.is_acyclic());
    }

    #[test]
    fn flatten_emits_levels_then_cycles() {
        let order = ParseOrder {
            levels: vec![vec!["a".into()], vec!["b".into()]],
            cycles: vec!["c".into()],
        };
        assert_eq!(order.flatten(), vec!["a", "b", "c"]);
        assert_eq!(order.total(), 3);
    }
}
