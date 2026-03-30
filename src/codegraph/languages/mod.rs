//! Language support registry for code graph indexing.
//!
//! Maps file extensions to supported languages and provides tree-sitter
//! parser configuration for each.

pub mod provider;
pub mod python;
pub mod rust_lang;
pub mod typescript;

use std::sync::Arc;

use provider::LanguageProvider;
use serde::{Deserialize, Serialize};

/// All languages supported by the code graph indexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportedLanguage {
    TypeScript,
    JavaScript,
    Python,
    Rust,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Ruby,
    Php,
    Kotlin,
    Swift,
    Markdown,
}

impl SupportedLanguage {
    /// Display name for UI.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TypeScript => "TypeScript",
            Self::JavaScript => "JavaScript",
            Self::Python => "Python",
            Self::Rust => "Rust",
            Self::Go => "Go",
            Self::Java => "Java",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::CSharp => "C#",
            Self::Ruby => "Ruby",
            Self::Php => "PHP",
            Self::Kotlin => "Kotlin",
            Self::Swift => "Swift",
            Self::Markdown => "Markdown",
        }
    }
}

impl std::fmt::Display for SupportedLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Map a file extension to a supported language.
pub fn language_for_extension(ext: &str) -> Option<SupportedLanguage> {
    match ext.to_lowercase().as_str() {
        "ts" | "tsx" | "mts" | "cts" => Some(SupportedLanguage::TypeScript),
        "js" | "jsx" | "mjs" | "cjs" => Some(SupportedLanguage::JavaScript),
        "py" | "pyi" | "pyw" => Some(SupportedLanguage::Python),
        "rs" => Some(SupportedLanguage::Rust),
        "go" => Some(SupportedLanguage::Go),
        "java" => Some(SupportedLanguage::Java),
        "c" | "h" => Some(SupportedLanguage::C),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => Some(SupportedLanguage::Cpp),
        "cs" => Some(SupportedLanguage::CSharp),
        "rb" | "rake" | "gemspec" => Some(SupportedLanguage::Ruby),
        "php" => Some(SupportedLanguage::Php),
        "kt" | "kts" => Some(SupportedLanguage::Kotlin),
        "swift" => Some(SupportedLanguage::Swift),
        "md" | "mdx" | "markdown" => Some(SupportedLanguage::Markdown),
        _ => None,
    }
}

/// Get the language provider for a given language.
pub fn provider_for(lang: SupportedLanguage) -> Option<Arc<dyn LanguageProvider>> {
    match lang {
        SupportedLanguage::TypeScript => Some(Arc::new(typescript::TypeScriptProvider)),
        SupportedLanguage::JavaScript => Some(Arc::new(typescript::JavaScriptProvider)),
        SupportedLanguage::Python => Some(Arc::new(python::PythonProvider)),
        SupportedLanguage::Rust => Some(Arc::new(rust_lang::RustProvider)),
        // Other languages will be added incrementally.
        _ => None,
    }
}

/// Get the language provider for a file based on its extension.
pub fn provider_for_extension(ext: &str) -> Option<Arc<dyn LanguageProvider>> {
    language_for_extension(ext).and_then(provider_for)
}

/// Return all known source file extensions.
pub fn all_extensions() -> &'static [&'static str] {
    &[
        "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "pyi",
        "pyw", "rs", "go", "java", "c", "h", "cpp", "cc", "cxx", "hpp",
        "hxx", "hh", "cs", "rb", "rake", "gemspec", "php", "kt", "kts",
        "swift", "md", "mdx", "markdown",
    ]
}
