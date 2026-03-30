//! Language provider trait for tree-sitter integration.
//!
//! Each supported language implements this trait to provide the tree-sitter
//! grammar and query patterns for symbol extraction.

use super::SupportedLanguage;
use crate::codegraph::types::NodeLabel;

/// A symbol extracted from a tree-sitter AST.
#[derive(Debug, Clone)]
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
    pub metadata: std::collections::HashMap<String, String>,
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

    /// Node labels that this language can produce.
    fn supported_labels(&self) -> &[NodeLabel];
}
