//! Language detection — maps file extensions and filenames to the
//! [`SupportedLanguage`] enum, and provides lookup functions for the
//! per-language provider implementations.
//!
//! Adding a new language is a one-line edit to the `LANGUAGES` table
//! below. The `language_for_extension`, `provider_for`,
//! `provider_for_extension`, and `all_extensions` functions all derive
//! their behavior from that single static table, so there is no risk
//! of forgetting to wire a language into one of several match arms.

use std::sync::Arc;

use super::languages::SupportedLanguage;
use super::provider::LanguageProvider;
use super::{
    c_cpp, c_sharp, cobol, dart, go, java, kotlin, php, python, ruby, rust_lang, swift, typescript,
};

/// A single row in the language registry.
///
/// Each entry ties a `SupportedLanguage` variant to the file extensions
/// and extensionless basenames that should be recognized as that
/// language, plus a factory that constructs the language's provider.
struct LanguageEntry {
    lang: SupportedLanguage,
    extensions: &'static [&'static str],
    extensionless_basenames: &'static [&'static str],
    factory: fn() -> Arc<dyn LanguageProvider>,
}

/// Single static table of every language supported by the code graph.
///
/// To add a new language, append one row here and declare the matching
/// provider module in `lang.rs`.
static LANGUAGES: &[LanguageEntry] = &[
    LanguageEntry {
        lang: SupportedLanguage::JavaScript,
        extensions: &["js", "jsx", "mjs", "cjs"],
        extensionless_basenames: &[],
        factory: || Arc::new(typescript::JavaScriptProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::TypeScript,
        extensions: &["ts", "tsx", "mts", "cts"],
        extensionless_basenames: &[],
        factory: || Arc::new(typescript::TypeScriptProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::Python,
        extensions: &["py", "pyi", "pyw"],
        extensionless_basenames: &[],
        factory: || Arc::new(python::PythonProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::Java,
        extensions: &["java"],
        extensionless_basenames: &[],
        factory: || Arc::new(java::JavaProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::C,
        extensions: &["c", "h"],
        extensionless_basenames: &[],
        factory: || Arc::new(c_cpp::CProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::Cpp,
        extensions: &["cpp", "cc", "cxx", "hpp", "hxx", "hh"],
        extensionless_basenames: &[],
        factory: || Arc::new(c_cpp::CppProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::CSharp,
        extensions: &["cs"],
        extensionless_basenames: &[],
        factory: || Arc::new(c_sharp::CSharpProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::Go,
        extensions: &["go"],
        extensionless_basenames: &[],
        factory: || Arc::new(go::GoProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::Ruby,
        extensions: &["rb", "rake", "gemspec"],
        extensionless_basenames: &[
            "Rakefile",
            "Gemfile",
            "Guardfile",
            "Vagrantfile",
            "Brewfile",
        ],
        factory: || Arc::new(ruby::RubyProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::Rust,
        extensions: &["rs"],
        extensionless_basenames: &[],
        factory: || Arc::new(rust_lang::RustProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::Php,
        extensions: &["php", "phtml", "php3", "php4", "php5", "php8"],
        extensionless_basenames: &[],
        factory: || Arc::new(php::PhpProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::Kotlin,
        extensions: &["kt", "kts"],
        extensionless_basenames: &[],
        factory: || Arc::new(kotlin::KotlinProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::Swift,
        extensions: &["swift"],
        extensionless_basenames: &[],
        factory: || Arc::new(swift::SwiftProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::Dart,
        extensions: &["dart"],
        extensionless_basenames: &[],
        factory: || Arc::new(dart::DartProvider),
    },
    LanguageEntry {
        lang: SupportedLanguage::Cobol,
        extensions: &["cbl", "cob", "cpy", "cobol"],
        extensionless_basenames: &[],
        factory: || Arc::new(cobol::CobolProvider),
    },
];

/// Map a file extension (without the leading dot) to a supported
/// language. Returns `None` if the extension is not recognized.
pub fn language_for_extension(ext: &str) -> Option<SupportedLanguage> {
    let ext_lower = ext.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|entry| entry.extensions.contains(&ext_lower.as_str()))
        .map(|entry| entry.lang)
}

/// Map a full filename (or path) to a supported language. Handles both
/// extension-based detection and extensionless basenames like
/// `Rakefile` or `Gemfile`.
pub fn language_for_filename(filename: &str) -> Option<SupportedLanguage> {
    // Fast path: extension-based detection.
    if let Some(dot) = filename.rfind('.') {
        let ext = &filename[dot + 1..];
        if let Some(lang) = language_for_extension(ext) {
            return Some(lang);
        }
    }
    // Slow path: extensionless filenames. Strip any directory prefix
    // using either Unix or Windows separators before comparing.
    let basename = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    LANGUAGES
        .iter()
        .find(|entry| entry.extensionless_basenames.contains(&basename))
        .map(|entry| entry.lang)
}

/// Construct a provider for the given language.
pub fn provider_for(lang: SupportedLanguage) -> Option<Arc<dyn LanguageProvider>> {
    LANGUAGES
        .iter()
        .find(|entry| entry.lang == lang)
        .map(|entry| (entry.factory)())
}

/// Convenience: construct a provider directly from a file extension.
pub fn provider_for_extension(ext: &str) -> Option<Arc<dyn LanguageProvider>> {
    language_for_extension(ext).and_then(provider_for)
}

/// Every known source-file extension across all supported languages.
pub fn all_extensions() -> Vec<&'static str> {
    LANGUAGES
        .iter()
        .flat_map(|entry| entry.extensions.iter().copied())
        .collect()
}
