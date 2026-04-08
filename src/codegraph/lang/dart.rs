//! Dart language provider.
//!
//! Extracts:
//! - `class_definition`    → Class (with `extends` superclass and `implements` interfaces)
//! - `mixin_declaration`   → Trait (Dart mixins map cleanly onto traits)
//! - `enum_declaration`    → Enum
//! - `function_signature`  → Function
//! - `method_signature`    → Method
//! - `import_or_export`    → Import
//! - `library_name`        → Module
//! - `getter_signature`, `setter_signature` → Variable
//!
//! Call extraction uses the enclosing-stack pattern to track the
//! current method/function qualified name, then walks through
//! `selector` / `invocation` / `function_expression_invocation` to
//! emit call sites with optional receiver information.

use super::languages::SupportedLanguage;
use super::provider::{CallSite, ExtractedSymbol, LanguageProvider};
use crate::codegraph::types::NodeLabel;

pub struct DartProvider;

impl LanguageProvider for DartProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Dart
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
        &["dart"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Class,
            NodeLabel::Trait,
            NodeLabel::Enum,
            NodeLabel::Function,
            NodeLabel::Method,
            NodeLabel::Variable,
            NodeLabel::Import,
            NodeLabel::Module,
        ]
    }
}

#[cfg(feature = "codegraph")]
fn dart_language() -> tree_sitter::Language {
    tree_sitter_dart::LANGUAGE.into()
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language = dart_language();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_dart_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_dart_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    match node.kind() {
        "library_name" => {
            let raw = node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .trim_start_matches("library")
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_string();
            if !raw.is_empty() {
                symbols.push(sym(file_path, None, &raw, NodeLabel::Module, &node));
            }
        }
        "import_or_export" | "import_specification" | "uri_based_directive" => {
            let raw = node.utf8_text(source.as_bytes()).unwrap_or("");
            // Extract the URI literal.
            if let Some(start) = raw.find(['"', '\''])
                && let Some(end) = raw[start + 1..].find(['"', '\''])
            {
                let path = raw[start + 1..start + 1 + end].to_string();
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
        "class_definition" => {
            if let Some(name) = find_ident(node, source, &["name", "identifier"]) {
                let qn = qname(file_path, parent_name, &name);
                let extends = node
                    .child_by_field_name("superclass")
                    .map(|n| text(n, source).replace("extends", "").trim().to_string());
                let implements = collect_interfaces(node, source);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qn.clone(),
                    label: NodeLabel::Class,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends,
                    implements,
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_dart_node(child, file_path, source, symbols, Some(&qn));
                }
                return;
            }
        }
        "mixin_declaration" => {
            if let Some(name) = find_ident(node, source, &["name", "identifier"]) {
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Trait, &node));
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_dart_node(child, file_path, source, symbols, Some(&qn));
                }
                return;
            }
        }
        "enum_declaration" => {
            if let Some(name) = find_ident(node, source, &["name", "identifier"]) {
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Enum, &node));
            }
        }
        "function_signature" => {
            if let Some(name) = find_ident(node, source, &["name", "identifier"]) {
                let label = if parent_name.is_some() {
                    NodeLabel::Method
                } else {
                    NodeLabel::Function
                };
                symbols.push(sym(file_path, parent_name, &name, label, &node));
            }
        }
        "method_signature" => {
            if let Some(name) = find_ident(node, source, &["name", "identifier"]) {
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Method, &node));
            }
        }
        "getter_signature" | "setter_signature" => {
            if let Some(name) = find_ident(node, source, &["name", "identifier"]) {
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Variable, &node));
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_dart_node(child, file_path, source, symbols, parent_name);
    }
}

#[cfg(feature = "codegraph")]
fn find_ident(
    node: tree_sitter::Node,
    source: &str,
    field_hints: &[&str],
) -> Option<String> {
    for hint in field_hints {
        if let Some(n) = node.child_by_field_name(hint) {
            let t = text(n, source);
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        let kind = child.kind();
        if matches!(kind, "identifier" | "type_identifier") {
            let t = text(child, source).trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

#[cfg(feature = "codegraph")]
fn collect_interfaces(class_node: tree_sitter::Node, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(impls) = class_node.child_by_field_name("interfaces") {
        let cursor = &mut impls.walk();
        for child in impls.children(cursor) {
            let t = text(child, source).trim().trim_matches(',').trim().to_string();
            if !t.is_empty() && t != "," && t != "implements" {
                out.push(t);
            }
        }
    }
    out
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(file_path: &str, content: &str) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language = dart_language();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_dart_calls(tree.root_node(), file_path, content, &mut calls, &mut Vec::new());
    calls
}

#[cfg(feature = "codegraph")]
fn walk_dart_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class_definition" | "mixin_declaration" | "enum_declaration" => {
            if let Some(name) = find_ident(node, source, &["name", "identifier"]) {
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_dart_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "function_signature" | "method_signature" => {
            if let Some(name) = find_ident(node, source, &["name", "identifier"]) {
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_dart_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "function_expression_invocation" | "invocation" | "method_invocation" => {
            if let Some(caller) = enclosing.last() {
                // Generic: first child is the function expression.
                if let Some(fn_expr) = node.child(0) {
                    let (name, receiver, is_method) = match fn_expr.kind() {
                        "identifier" => (text(fn_expr, source), None, false),
                        _ => {
                            // Best-effort: last identifier in the expression.
                            let name = last_identifier_text(fn_expr, source);
                            let recv = fn_expr.child(0).map(|n| text(n, source));
                            (name, recv, true)
                        }
                    };
                    if !name.is_empty() {
                        calls.push(CallSite {
                            caller_qualified_name: caller.clone(),
                            callee_name: name,
                            line: node.start_position().row as u32 + 1,
                            is_method_call: is_method,
                            receiver,
                        });
                    }
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_dart_calls(child, file_path, source, calls, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn last_identifier_text(node: tree_sitter::Node, source: &str) -> String {
    let mut result = String::new();
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        if child.kind() == "identifier" {
            result = text(child, source);
        }
    }
    result
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
            ("mixin ", NodeLabel::Trait),
            ("enum ", NodeLabel::Enum),
        ];
        for (prefix, label) in patterns {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
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
