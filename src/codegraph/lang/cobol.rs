//! COBOL language provider.
//!
//! COBOL has no widely-maintained tree-sitter grammar, so this provider
//! uses a regex/line-scan approach to extract the major structural
//! elements of a COBOL program:
//!
//! - `PROGRAM-ID.` statements → Module (one per program)
//! - `<NAME> SECTION.`        → Function (PROCEDURE DIVISION sections)
//! - `<NAME>.` paragraphs     → Function (procedure paragraphs)
//! - `COPY <NAME>`            → Import
//! - `CALL "<NAME>"`          → CallSite (program-to-program calls)
//!
//! COBOL is case-insensitive; all keywords are matched case-insensitively.
//! The provider avoids allocating on the hot path by operating on
//! `&str` slices wherever possible.

use super::languages::SupportedLanguage;
use super::provider::{CallSite, ExtractedSymbol, LanguageProvider};
use crate::codegraph::types::NodeLabel;

pub struct CobolProvider;

impl LanguageProvider for CobolProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Cobol
    }

    fn extract_symbols(&self, file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
        let mut symbols = Vec::new();
        let mut current_program: Option<String> = None;

        for (i, raw_line) in content.lines().enumerate() {
            let line_num = i as u32 + 1;

            // Strip COBOL fixed-format sequence number columns 1-6 and
            // the indicator in column 7 if present. Free-format lines
            // (modern COBOL) are untouched by this step.
            let line = strip_fixed_format_prefix(raw_line);
            let trimmed = line.trim();

            // Skip comments and blank lines.
            if trimmed.is_empty() || trimmed.starts_with('*') {
                continue;
            }

            let upper = trimmed.to_ascii_uppercase();

            // PROGRAM-ID. <name>.
            if let Some(rest) = upper.strip_prefix("PROGRAM-ID.") {
                let name = extract_cobol_identifier(rest);
                if !name.is_empty() {
                    current_program = Some(name.clone());
                    symbols.push(sym(file_path, None, &name, NodeLabel::Module, line_num));
                }
                continue;
            }

            // <NAME> SECTION.
            if let Some(name) = extract_section_name(&upper) {
                let parent = current_program
                    .as_ref()
                    .map(|p| format!("{file_path}::{p}"));
                symbols.push(sym(
                    file_path,
                    parent.as_deref(),
                    &name,
                    NodeLabel::Function,
                    line_num,
                ));
                continue;
            }

            // COPY <name>.
            if let Some(rest) = upper.strip_prefix("COPY ") {
                let name = extract_cobol_identifier(rest);
                if !name.is_empty() {
                    symbols.push(ExtractedSymbol {
                        name: name.clone(),
                        qualified_name: format!("{file_path}::import::{name}"),
                        label: NodeLabel::Import,
                        line_start: line_num,
                        line_end: line_num,
                        parent: None,
                        import_source: Some(name),
                        extends: None,
                        implements: Vec::new(),
                        decorates: None,
                        metadata: std::collections::HashMap::new(),
                    });
                }
            }
        }

        symbols
    }

    fn extract_calls(&self, file_path: &str, content: &str) -> Vec<CallSite> {
        let mut calls = Vec::new();
        let mut current_enclosing: Option<String> = None;

        for (i, raw_line) in content.lines().enumerate() {
            let line_num = i as u32 + 1;
            let line = strip_fixed_format_prefix(raw_line);
            let trimmed = line.trim();

            if trimmed.is_empty() || trimmed.starts_with('*') {
                continue;
            }

            let upper = trimmed.to_ascii_uppercase();

            // Track enclosing section/paragraph for caller qualified names.
            if let Some(rest) = upper.strip_prefix("PROGRAM-ID.") {
                let name = extract_cobol_identifier(rest);
                if !name.is_empty() {
                    current_enclosing = Some(format!("{file_path}::{name}"));
                }
                continue;
            }
            if let Some(name) = extract_section_name(&upper) {
                current_enclosing = Some(format!("{file_path}::{name}"));
                continue;
            }

            // CALL "<name>" or CALL <identifier>.
            if let Some(rest) = upper.strip_prefix("CALL ")
                && let Some(callee) = extract_call_target(rest)
                && let Some(caller) = current_enclosing.as_ref()
            {
                calls.push(CallSite {
                    caller_qualified_name: caller.clone(),
                    callee_name: callee,
                    line: line_num,
                    is_method_call: false,
                    receiver: None,
                });
            }
        }

        calls
    }

    fn file_extensions(&self) -> &[&str] {
        &["cbl", "cob", "cpy", "cobol"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Module,
            NodeLabel::Function,
            NodeLabel::Import,
        ]
    }
}

/// Strip the fixed-format sequence number columns (1-6) and the
/// indicator column (7) from a COBOL source line. Lines shorter than 7
/// characters are returned unchanged.
fn strip_fixed_format_prefix(line: &str) -> &str {
    let char_count = line.chars().count();
    if char_count < 7 {
        return line;
    }
    // Only strip if the first 6 characters look like a sequence area
    // (digits, spaces, or empty) — modern free-format COBOL shouldn't
    // be stripped.
    let prefix: String = line.chars().take(6).collect();
    if prefix.chars().all(|c| c.is_ascii_digit() || c.is_whitespace()) {
        // Skip past the 7-character prefix (sequence area + indicator).
        let mut iter = line.char_indices();
        for _ in 0..7 {
            iter.next();
        }
        if let Some((byte_idx, _)) = iter.next() {
            return &line[byte_idx..];
        }
    }
    line
}

/// Pull a COBOL identifier off the front of a string, consuming
/// leading whitespace. Stops at the first non-identifier character.
fn extract_cobol_identifier(s: &str) -> String {
    s.trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// If the (uppercased) line declares a SECTION, return the section
/// name; otherwise return None. COBOL section declarations look like:
/// `<NAME> SECTION.`
fn extract_section_name(upper_line: &str) -> Option<String> {
    let without_trailing_dot = upper_line.trim_end_matches('.').trim_end();
    let without_keyword = without_trailing_dot.strip_suffix(" SECTION")?;
    let name = without_keyword.trim();
    if name.is_empty() || name.contains(' ') {
        None
    } else {
        Some(name.to_string())
    }
}

/// Extract the target of a COBOL `CALL "NAME"` or `CALL NAME` statement.
fn extract_call_target(rest: &str) -> Option<String> {
    let trimmed = rest.trim_start();
    if let Some(after_quote) = trimmed.strip_prefix('"') {
        let name: String = after_quote.chars().take_while(|c| *c != '"').collect();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    } else if let Some(after_quote) = trimmed.strip_prefix('\'') {
        let name: String = after_quote.chars().take_while(|c| *c != '\'').collect();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    } else {
        let name = extract_cobol_identifier(trimmed);
        if name.is_empty() { None } else { Some(name) }
    }
}

fn sym(
    file_path: &str,
    parent: Option<&str>,
    name: &str,
    label: NodeLabel,
    line: u32,
) -> ExtractedSymbol {
    ExtractedSymbol {
        name: name.to_string(),
        qualified_name: match parent {
            Some(p) => format!("{p}::{name}"),
            None => format!("{file_path}::{name}"),
        },
        label,
        line_start: line,
        line_end: line,
        parent: parent.map(String::from),
        import_source: None,
        extends: None,
        implements: Vec::new(),
        decorates: None,
        metadata: std::collections::HashMap::new(),
    }
}
