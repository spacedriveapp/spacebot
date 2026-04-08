//! Supported languages — single source of truth for which languages
//! the code graph indexer can process.
//!
//! Both the ingestion pipeline and downstream consumers use this to
//! identify which language a file, symbol, or call belongs to.

use serde::{Deserialize, Serialize};

/// Every language supported by the code graph indexer.
///
/// When adding a new variant, also update the `LANGUAGES` table in
/// `language_detection.rs` and create a provider module under
/// `src/codegraph/lang/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportedLanguage {
    JavaScript,
    TypeScript,
    Python,
    Java,
    C,
    Cpp,
    CSharp,
    Go,
    Ruby,
    Rust,
    Php,
    Kotlin,
    Swift,
    Dart,
    /// COBOL uses a regex-based provider rather than tree-sitter.
    Cobol,
}

impl SupportedLanguage {
    /// Human-readable display name used in UI and logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::JavaScript => "JavaScript",
            Self::TypeScript => "TypeScript",
            Self::Python => "Python",
            Self::Java => "Java",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::CSharp => "C#",
            Self::Go => "Go",
            Self::Ruby => "Ruby",
            Self::Rust => "Rust",
            Self::Php => "PHP",
            Self::Kotlin => "Kotlin",
            Self::Swift => "Swift",
            Self::Dart => "Dart",
            Self::Cobol => "COBOL",
        }
    }
}

impl std::fmt::Display for SupportedLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
