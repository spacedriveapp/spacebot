//! Language provider trait for tree-sitter integration.
//!
//! Each supported language implements this trait to provide the tree-sitter
//! grammar and query patterns for symbol extraction.

use super::languages::SupportedLanguage;
use crate::codegraph::types::NodeLabel;

/// A call site extracted from the AST.
#[derive(Debug, Clone)]
pub struct CallSite {
    /// Qualified name of the enclosing function/method containing this call.
    pub caller_qualified_name: String,
    /// Name being called (function or method name).
    pub callee_name: String,
    /// Line number of the call (1-based).
    pub line: u32,
    /// Whether this is a method call (`obj.method()`) vs bare function call (`func()`).
    pub is_method_call: bool,
    /// For method calls: the receiver expression text (e.g., "self", "obj", "ClassName").
    pub receiver: Option<String>,
}

/// A typed local variable binding inside a function body.
///
/// Consumed only by the call-site resolver as an in-memory extension
/// of the type environment — no graph nodes are produced, so locals
/// don't inflate the node count on large projects.
#[derive(Debug, Clone)]
pub struct LocalBinding {
    /// Qualified name of the enclosing function or method.
    pub function_qualified_name: String,
    /// Local variable name as written in source.
    pub name: String,
    /// Source-level type expression (e.g. `Foo`, `Arc<Mutex<Bar>>`).
    pub declared_type: String,
}

/// A field access extracted from the AST — `self.x`, `this.foo`, etc.
///
/// Resolved later in the accesses phase into an ACCESSES edge from the
/// enclosing function/method to the corresponding Variable node.
#[derive(Debug, Clone)]
pub struct AccessSite {
    /// Qualified name of the enclosing function/method doing the access.
    pub caller_qualified_name: String,
    /// Name of the field being read or written (the part after the dot).
    pub field_name: String,
    /// Receiver expression text (e.g. "self", "this", "obj").
    pub receiver: String,
    /// Line number of the access (1-based).
    pub line: u32,
    /// True if the access is on the LHS of an assignment (write),
    /// false if it's a read. Currently informational only — both
    /// produce ACCESSES edges.
    pub is_write: bool,
}

/// A symbol extracted from a tree-sitter AST.
#[derive(Debug, Clone, Default)]
pub struct ExtractedSymbol {
    /// Symbol name.
    pub name: String,
    /// Qualified name (e.g., `ClassName::method_name`).
    pub qualified_name: String,
    /// What kind of symbol this is.
    pub label: NodeLabel,
    /// Start line (1-based).
    pub line_start: u32,
    /// End line (1-based).
    pub line_end: u32,
    /// Parent symbol qualified name (for nesting).
    pub parent: Option<String>,
    /// For imports: the source module path.
    pub import_source: Option<String>,
    /// For classes: what they extend.
    pub extends: Option<String>,
    /// For classes: what they implement.
    pub implements: Vec<String>,
    /// For decorators: what they decorate.
    pub decorates: Option<String>,
    /// Additional language-specific metadata.
    ///
    /// Known keys read by the pipeline:
    /// - `"declared_type"`: for Parameters and typed Variables, the
    ///   source-level type expression (e.g. `"Foo"`, `"Arc<Mutex<T>>"`,
    ///   `"map[string]int"`). The call-site resolver uses this to bind
    ///   receivers to their class qnames for method lookup.
    pub metadata: std::collections::HashMap<String, String>,
    /// Whether this symbol is exported / public API.
    pub is_exported: bool,
    /// Return type string for functions/methods.
    pub return_type: Option<String>,
    /// Visibility: "public", "private", "protected", or "internal".
    pub visibility: Option<String>,
    /// Number of parameters (functions/methods/constructors).
    pub parameter_count: Option<u32>,
    /// Whether this symbol is static.
    pub is_static: bool,
    /// Whether this symbol is readonly/const/final.
    pub is_readonly: bool,
    /// Whether this symbol is abstract.
    pub is_abstract: bool,
    /// Whether this symbol is final/sealed.
    pub is_final: bool,
    /// Comma-separated annotation/attribute names.
    pub annotations: Option<String>,
}

/// Trait that each language provider implements.
///
/// Provides tree-sitter grammar and symbol extraction patterns.
pub trait LanguageProvider: Send + Sync {
    /// Which language this provider handles.
    fn language(&self) -> SupportedLanguage;

    /// Extract symbols from source code.
    ///
    /// Takes the file content and returns all symbols found.
    fn extract_symbols(
        &self,
        file_path: &str,
        content: &str,
    ) -> Vec<ExtractedSymbol>;

    /// Extract call sites from source code using AST analysis.
    ///
    /// Returns structured call sites with caller context, callee name,
    /// and receiver info for method calls. Default returns empty.
    fn extract_calls(&self, _file_path: &str, _content: &str) -> Vec<CallSite> {
        Vec::new()
    }

    /// Extract field-access sites from source code using AST analysis.
    ///
    /// Returns sites where a function or method reads/writes a field on
    /// `self`, `this`, or another receiver. Default returns empty so
    /// providers that haven't implemented this opt out cleanly.
    fn extract_accesses(&self, _file_path: &str, _content: &str) -> Vec<AccessSite> {
        Vec::new()
    }

    /// Extract typed local variable bindings from function bodies.
    ///
    /// Only locals with an explicit type annotation are emitted;
    /// inferred types would require flow analysis. Default returns
    /// empty so untyped languages opt out cleanly.
    fn extract_locals(&self, _file_path: &str, _content: &str) -> Vec<LocalBinding> {
        Vec::new()
    }

    /// Identify test functions in this file by qualified name.
    ///
    /// Providers use language-specific heuristics (attributes like
    /// `#[test]` / `@Test`, naming conventions like `Test*` / `test_*`,
    /// or receiver-type signatures) to classify functions as tests.
    /// The resolver emits TESTED_BY edges from a call site's callee
    /// back to any caller that appears in this set.
    fn extract_tests(&self, _file_path: &str, _content: &str) -> Vec<String> {
        Vec::new()
    }

    /// File extensions (without the leading dot) that this provider
    /// handles. Kept on the trait so extension→provider routing can
    /// eventually live on each provider instead of the central
    /// `LANGUAGES` table in `language_detection.rs`; the default empty
    /// slice keeps existing providers backwards compatible until they
    /// are updated one at a time.
    fn file_extensions(&self) -> &[&str] {
        &[]
    }

    /// Node labels that this language can produce.
    fn supported_labels(&self) -> &[NodeLabel];

    /// Declarative tree-sitter queries for this provider.
    ///
    /// Returning `Some(q)` opts the provider into the Phase 2+ query-
    /// based extraction path — each `Option<&'static str>` inside the
    /// [`crate::codegraph::lang::queries::QuerySet`] lets a provider
    /// migrate one extraction kind at a time without rewriting the
    /// whole walk. `None` (the default) keeps the walk-based path
    /// active, which is how every provider ships today.
    fn queries(&self) -> Option<&'static super::queries::QuerySet> {
        None
    }
}
