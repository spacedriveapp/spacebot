//! Language support for code graph indexing.
//!
//! Provides per-language symbol and call-site extraction via tree-sitter
//! (with a regex fallback when the `codegraph` feature is disabled or the
//! parser fails), plus a registry that maps file extensions and
//! extensionless basenames to the right provider.
//!
//! Adding a new language:
//! 1. Add a variant to [`SupportedLanguage`] in `languages.rs`
//! 2. Create a provider file under `src/codegraph/lang/`
//! 3. Declare the new module below with `pub mod <name>;`
//! 4. Add a row to the `LANGUAGES` table in `language_detection.rs`

#[cfg(feature = "codegraph")]
pub mod ast_cache;
pub mod cobol_exec;
pub mod cobol_preprocessor;
pub mod jcl;
pub mod parse_order;
pub mod provider;
pub mod queries;

// Registry and enum.
pub mod language_detection;
pub mod languages;

// Per-language providers.
pub mod c_cpp;
pub mod c_sharp;
pub mod cobol;
pub mod dart;
pub mod go;
pub mod html;
pub mod java;
pub mod kotlin;
pub mod php;
pub mod prisma;
pub mod python;
pub mod ruby;
pub mod rust_lang;
pub mod swift;
pub mod typescript;

pub use language_detection::{
    all_extensions, language_for_extension, language_for_filename, provider_for,
    provider_for_extension,
};
pub use languages::SupportedLanguage;
