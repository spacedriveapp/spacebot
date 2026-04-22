//! Declarative tree-sitter query scaffolding for Phase 2+ extraction.
//!
//! GitNexus's extractors are driven by S-expression queries — tree-
//! sitter compiles each query once, then `QueryCursor` finds every
//! match in a single pass. Our legacy providers walk the AST
//! manually, duplicating logic across 13 languages and diverging in
//! subtle ways. The per-language submodules here hold the query text
//! (ported verbatim from GitNexus) and the top-level [`QuerySet`]
//! bundles them for the [`super::provider::LanguageProvider::queries`]
//! hook.
//!
//! ## Capture vocabulary
//!
//! Every query sticks to a shared capture-tag vocabulary so a single
//! runner can drive extraction independently of the language:
//!
//! - `@name` — bare symbol name inside a definition match.
//! - `@definition.class` / `.function` / `.method` / `.property` /
//!   `.struct` / `.enum` / `.interface` / `.trait` / `.impl` /
//!   `.namespace` / `.module` / `.type` / `.const` / `.static` /
//!   `.macro` / `.typedef` / `.union` / `.record` / `.delegate` /
//!   `.template` / `.annotation` / `.constructor` — match kind.
//! - `@import.source` — import path inside an import statement.
//! - `@call.name` — callee identifier.
//! - `@heritage.class` / `.extends` / `.implements` / `.trait` —
//!   inheritance.
//! - `@assignment.receiver` / `.property` — write-site captures.
//! - `@decorator.name` / `.arg` / `.receiver` — decorator sites.
//! - `@http_client.method` / `.url` / `@route.fetch` / `@route.url` /
//!   `@express_route.method` / `.path` — HTTP-consumer patterns.

pub mod c;
pub mod cpp;
pub mod csharp;
pub mod dart;
pub mod go;
pub mod java;
pub mod javascript;
pub mod kotlin;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust;
pub mod swift;
pub mod typescript;

/// A bundle of tree-sitter query sources, one per extraction kind.
///
/// Each field holds the raw S-expression text. `None` means "no
/// query available yet for this language — caller must fall back to
/// the walk-based path." As provider migrations land the `fields` /
/// `tests` / `locals` slots get populated; today we emit the same
/// block in `symbols` / `calls` / `accesses` / `exports` because
/// GitNexus groups all of them into one query per language and the
/// runner dispatches by capture tag.
#[derive(Debug, Clone, Copy, Default)]
pub struct QuerySet {
    pub symbols: Option<&'static str>,
    pub calls: Option<&'static str>,
    pub accesses: Option<&'static str>,
    pub locals: Option<&'static str>,
    pub tests: Option<&'static str>,
    pub fields: Option<&'static str>,
    pub exports: Option<&'static str>,
}

/// Compose a [`QuerySet`] in which every "has a query" slot points at
/// the same GitNexus-style bundled source. Locals and tests stay
/// `None` because GitNexus doesn't bundle them into the main query.
pub(crate) const fn bundle(source: &'static str) -> QuerySet {
    QuerySet {
        symbols: Some(source),
        calls: Some(source),
        accesses: Some(source),
        locals: None,
        tests: None,
        fields: Some(source),
        exports: Some(source),
    }
}

/// Run a compiled tree-sitter query against a parsed tree and invoke
/// `on_match` for every match. The callback receives a slice of
/// `(capture_name, node)` tuples so language-specific extractors can
/// dispatch on the capture tag.
///
/// The runner exists so every provider migration reuses the same
/// streaming-iterator boilerplate; downstream extractors (symbol
/// builders, heritage scanners, HTTP-consumer detectors) layer on top
/// of this primitive.
#[cfg(feature = "codegraph")]
pub fn run_query<F>(
    source: &[u8],
    tree: &tree_sitter::Tree,
    query: &tree_sitter::Query,
    mut on_match: F,
) where
    F: FnMut(&[(&str, tree_sitter::Node<'_>)]),
{
    use streaming_iterator::StreamingIterator;

    let mut cursor = tree_sitter::QueryCursor::new();
    let names = query.capture_names();
    let mut buf: Vec<(&str, tree_sitter::Node<'_>)> = Vec::new();

    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(m) = matches.next() {
        buf.clear();
        for cap in m.captures {
            let name = names.get(cap.index as usize).copied().unwrap_or("");
            buf.push((name, cap.node));
        }
        on_match(&buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_query_set_has_no_sources() {
        let qs = QuerySet::default();
        assert!(qs.symbols.is_none());
        assert!(qs.calls.is_none());
        assert!(qs.accesses.is_none());
        assert!(qs.locals.is_none());
        assert!(qs.tests.is_none());
        assert!(qs.fields.is_none());
        assert!(qs.exports.is_none());
    }

    #[test]
    fn query_set_is_copy() {
        let qs = QuerySet {
            symbols: Some("(function_definition) @sym"),
            ..Default::default()
        };
        let clone = qs;
        assert_eq!(qs.symbols, clone.symbols);
    }

    #[test]
    fn bundle_helper_fills_populated_slots() {
        let qs = bundle("(function_definition) @sym");
        assert!(qs.symbols.is_some());
        assert!(qs.calls.is_some());
        assert!(qs.accesses.is_some());
        assert!(qs.fields.is_some());
        assert!(qs.exports.is_some());
        assert!(qs.locals.is_none());
        assert!(qs.tests.is_none());
    }

    #[cfg(feature = "codegraph")]
    #[test]
    fn run_query_captures_top_level_symbols() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let source = "def foo():\n    pass\n\nclass Bar:\n    pass\n";
        let tree = parser.parse(source, None).expect("parse");
        let query = tree_sitter::Query::new(
            &tree_sitter_python::LANGUAGE.into(),
            "(function_definition name: (identifier) @fn)\n(class_definition name: (identifier) @cls)",
        )
        .expect("compile");

        let mut names: Vec<String> = Vec::new();
        run_query(source.as_bytes(), &tree, &query, |caps| {
            for (label, node) in caps {
                names.push(format!(
                    "{label}={}",
                    node.utf8_text(source.as_bytes()).unwrap()
                ));
            }
        });
        names.sort();
        assert_eq!(names, vec!["cls=Bar", "fn=foo"]);
    }
}
