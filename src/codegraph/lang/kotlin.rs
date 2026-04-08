//! Kotlin language provider.
//!
//! Extracts:
//! - `class_declaration`     → Class (handles `object` and `companion object` too)
//! - `object_declaration`    → Class (Kotlin singleton)
//! - `interface_declaration` → Interface
//! - `function_declaration`  → Function (top level) / Method (nested)
//! - `property_declaration`  → Variable
//! - `package_header`        → Module
//! - `import_header`         → Import
//!
//! Call extraction walks `call_expression` nodes. Kotlin method calls
//! typically appear as `navigation_expression` on the expression side,
//! which we unpack to extract receiver + callee name.

use super::languages::SupportedLanguage;
use super::provider::{CallSite, ExtractedSymbol, LanguageProvider};
use crate::codegraph::types::NodeLabel;

pub struct KotlinProvider;

impl LanguageProvider for KotlinProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Kotlin
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
        &["kt", "kts"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Class,
            NodeLabel::Interface,
            NodeLabel::Function,
            NodeLabel::Method,
            NodeLabel::Variable,
            NodeLabel::Module,
            NodeLabel::Import,
        ]
    }
}

#[cfg(feature = "codegraph")]
fn kotlin_language() -> tree_sitter::Language {
    tree_sitter_kotlin_ng::LANGUAGE.into()
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language = kotlin_language();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_kotlin_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_kotlin_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    match node.kind() {
        "package_header" => {
            let raw = node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .trim_start_matches("package")
                .trim()
                .to_string();
            if !raw.is_empty() {
                symbols.push(sym_line(file_path, None, &raw, NodeLabel::Module, &node));
            }
        }
        "import_header" => {
            let raw = node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .trim_start_matches("import")
                .trim()
                .to_string();
            if !raw.is_empty() {
                symbols.push(ExtractedSymbol {
                    name: raw.clone(),
                    qualified_name: format!("{file_path}::import::{raw}"),
                    label: NodeLabel::Import,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: None,
                    import_source: Some(raw),
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
            }
        }
        "class_declaration" | "object_declaration" => {
            let name = find_name_child(node, source);
            if !name.is_empty() {
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Class, &node));
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_kotlin_node(child, file_path, source, symbols, Some(&qn));
                }
                return;
            }
        }
        "interface_declaration" => {
            let name = find_name_child(node, source);
            if !name.is_empty() {
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Interface, &node));
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_kotlin_node(child, file_path, source, symbols, Some(&qn));
                }
                return;
            }
        }
        "function_declaration" => {
            let name = find_name_child(node, source);
            if !name.is_empty() {
                let label = if parent_name.is_some() {
                    NodeLabel::Method
                } else {
                    NodeLabel::Function
                };
                symbols.push(sym(file_path, parent_name, &name, label, &node));
            }
        }
        "property_declaration" => {
            let name = find_name_child(node, source);
            if !name.is_empty() {
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Variable, &node));
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_kotlin_node(child, file_path, source, symbols, parent_name);
    }
}

/// Find the first `simple_identifier` or `type_identifier` child,
/// which Kotlin's tree-sitter grammar uses for declaration names.
#[cfg(feature = "codegraph")]
fn find_name_child(node: tree_sitter::Node, source: &str) -> String {
    if let Some(n) = node.child_by_field_name("name") {
        return text(n, source);
    }
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        let kind = child.kind();
        if kind == "simple_identifier" || kind == "type_identifier" || kind == "identifier" {
            return text(child, source);
        }
    }
    String::new()
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(file_path: &str, content: &str) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language = kotlin_language();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_kotlin_calls(tree.root_node(), file_path, content, &mut calls, &mut Vec::new());
    calls
}

#[cfg(feature = "codegraph")]
fn walk_kotlin_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class_declaration" | "object_declaration" | "interface_declaration" => {
            let name = find_name_child(node, source);
            if !name.is_empty() {
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_kotlin_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "function_declaration" => {
            let name = find_name_child(node, source);
            if !name.is_empty() {
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_kotlin_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "call_expression" => {
            if let Some(caller) = enclosing.last() {
                // Kotlin grammar: first child is the callee expression,
                // which may be a simple_identifier (bare call) or a
                // navigation_expression (receiver.method).
                if let Some(callee_expr) = node.child(0) {
                    let (name, receiver, is_method) = match callee_expr.kind() {
                        "simple_identifier" | "identifier" => {
                            (text(callee_expr, source), None, false)
                        }
                        "navigation_expression" => {
                            // navigation_expression: expression navigation_suffix
                            // navigation_suffix: "." simple_identifier
                            let recv = callee_expr.child(0).map(|n| text(n, source));
                            let name = find_last_identifier(callee_expr, source);
                            (name, recv, true)
                        }
                        _ => (text(callee_expr, source), None, false),
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
        walk_kotlin_calls(child, file_path, source, calls, enclosing);
    }
}

/// Walk a navigation expression (recursive) to find the final
/// simple_identifier — this is the called method's name.
#[cfg(feature = "codegraph")]
fn find_last_identifier(node: tree_sitter::Node, source: &str) -> String {
    let mut result = String::new();
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        if child.kind() == "simple_identifier" || child.kind() == "identifier" {
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

#[cfg(feature = "codegraph")]
fn sym_line(
    file_path: &str,
    parent: Option<&str>,
    name: &str,
    label: NodeLabel,
    node: &tree_sitter::Node,
) -> ExtractedSymbol {
    sym(file_path, parent, name, label, node)
}

fn extract_fallback(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = i as u32 + 1;

        let patterns: &[(&str, NodeLabel)] = &[
            ("class ", NodeLabel::Class),
            ("object ", NodeLabel::Class),
            ("interface ", NodeLabel::Interface),
            ("fun ", NodeLabel::Function),
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
