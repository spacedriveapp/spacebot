//! Ruby language provider.
//!
//! Extracts:
//! - `class`            → Class (with superclass → `extends`)
//! - `module`           → Module
//! - `method`           → Method / Function (depending on nesting)
//! - `singleton_method` → Method (class-level `def self.foo`)
//! - `assignment` at the top level with a Constant lhs → Const
//! - `call` to `require`/`require_relative`/`load`/`autoload` → Import
//!
//! Call extraction walks `call` nodes, which carry a `method` field
//! and an optional `receiver` field. Bare calls (no receiver) become
//! `is_method_call=false`; anything with a receiver becomes a method
//! call with receiver text recorded for downstream resolution.

use super::languages::SupportedLanguage;
use super::provider::{CallSite, ExtractedSymbol, LanguageProvider};
use crate::codegraph::types::NodeLabel;

pub struct RubyProvider;

impl LanguageProvider for RubyProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Ruby
    }

    fn extract_symbols(&self, file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
        #[cfg(feature = "codegraph")]
        {
            extract_with_tree_sitter(file_path, content)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            extract_fallback(file_path, content)
        }
    }

    fn extract_calls(&self, file_path: &str, content: &str) -> Vec<CallSite> {
        #[cfg(feature = "codegraph")]
        {
            extract_calls_tree_sitter(file_path, content)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            let _ = (file_path, content);
            Vec::new()
        }
    }

    fn file_extensions(&self) -> &[&str] {
        &["rb", "rake", "gemspec"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Class,
            NodeLabel::Module,
            NodeLabel::Method,
            NodeLabel::Function,
            NodeLabel::Const,
            NodeLabel::Import,
        ]
    }
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_ruby::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_ruby_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_ruby_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    match node.kind() {
        "class" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                let extends = node
                    .child_by_field_name("superclass")
                    .map(|n| text(n, source).trim_start_matches('<').trim().to_string());
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qn.clone(),
                    label: NodeLabel::Class,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends,
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_ruby_node(child, file_path, source, symbols, Some(&qn));
                }
                return;
            }
        }
        "module" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Module, &node));
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_ruby_node(child, file_path, source, symbols, Some(&qn));
                }
                return;
            }
        }
        "method" | "singleton_method" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    let label = if parent_name.is_some() {
                        NodeLabel::Method
                    } else {
                        NodeLabel::Function
                    };
                    symbols.push(sym(file_path, parent_name, &name, label, &node));
                }
            }
        }
        "call" => {
            // Detect require / require_relative / load / autoload.
            let method_node = node.child_by_field_name("method");
            let receiver = node.child_by_field_name("receiver");
            if receiver.is_none()
                && let Some(m) = method_node
            {
                let method_name = text(m, source);
                if matches!(
                    method_name.as_str(),
                    "require" | "require_relative" | "load" | "autoload"
                ) && let Some(args) = node.child_by_field_name("arguments")
                {
                    let raw = text(args, source);
                    let path = raw
                        .trim_matches(|c| c == '(' || c == ')')
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string();
                    if !path.is_empty() {
                        symbols.push(ExtractedSymbol {
                            name: path.clone(),
                            qualified_name: format!("{file_path}::import::{path}"),
                            label: NodeLabel::Import,
                            line_start: node.start_position().row as u32 + 1,
                            line_end: node.end_position().row as u32 + 1,
                            parent: None,
                            import_source: Some(path),
                            extends: None,
                            implements: Vec::new(),
                            decorates: None,
                            metadata: std::collections::HashMap::new(),
                        });
                    }
                }
            }
        }
        "assignment" => {
            // Top-level constant assignments: FOO = 123
            if parent_name.is_none()
                && let Some(left) = node.child_by_field_name("left")
                && left.kind() == "constant"
            {
                let name = text(left, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, None, &name, NodeLabel::Const, &node));
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_ruby_node(child, file_path, source, symbols, parent_name);
    }
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(file_path: &str, content: &str) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_ruby::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_ruby_calls(tree.root_node(), file_path, content, &mut calls, &mut Vec::new());
    calls
}

#[cfg(feature = "codegraph")]
fn walk_ruby_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class" | "module" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_ruby_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "method" | "singleton_method" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_ruby_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "call" => {
            if let Some(caller) = enclosing.last()
                && let Some(method_node) = node.child_by_field_name("method")
            {
                let name = text(method_node, source);
                let receiver = node.child_by_field_name("receiver").map(|n| text(n, source));
                if !name.is_empty() {
                    calls.push(CallSite {
                        caller_qualified_name: caller.clone(),
                        callee_name: name,
                        line: node.start_position().row as u32 + 1,
                        is_method_call: receiver.is_some(),
                        receiver,
                    });
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_ruby_calls(child, file_path, source, calls, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn text(node: tree_sitter::Node, source: &str) -> String {
    node.utf8_text(source.as_bytes()).unwrap_or("").to_string()
}

#[cfg(feature = "codegraph")]
fn qname(file_path: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}::{name}"),
        None => format!("{file_path}::{name}"),
    }
}

#[cfg(feature = "codegraph")]
fn sym(
    file_path: &str,
    parent: Option<&str>,
    name: &str,
    label: NodeLabel,
    node: &tree_sitter::Node,
) -> ExtractedSymbol {
    ExtractedSymbol {
        name: name.to_string(),
        qualified_name: qname(file_path, parent, name),
        label,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        parent: parent.map(String::from),
        import_source: None,
        extends: None,
        implements: Vec::new(),
        decorates: None,
        metadata: std::collections::HashMap::new(),
    }
}

fn extract_fallback(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = i as u32 + 1;

        let patterns: &[(&str, NodeLabel)] = &[
            ("class ", NodeLabel::Class),
            ("module ", NodeLabel::Module),
            ("def ", NodeLabel::Method),
        ];
        for (prefix, label) in patterns {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let name: String = rest
                    .chars()
                    .take_while(|c| {
                        c.is_alphanumeric() || *c == '_' || *c == '?' || *c == '!' || *c == '='
                    })
                    .collect();
                if !name.is_empty() {
                    symbols.push(fallback_sym(file_path, &name, *label, line_num));
                    break;
                }
            }
        }
    }
    symbols
}

fn fallback_sym(file_path: &str, name: &str, label: NodeLabel, line: u32) -> ExtractedSymbol {
    ExtractedSymbol {
        name: name.to_string(),
        qualified_name: format!("{file_path}::{name}"),
        label,
        line_start: line,
        line_end: line,
        parent: None,
        import_source: None,
        extends: None,
        implements: Vec::new(),
        decorates: None,
        metadata: std::collections::HashMap::new(),
    }
}
