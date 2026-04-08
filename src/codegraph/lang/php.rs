//! PHP language provider.
//!
//! Extracts:
//! - `class_declaration`     → Class (with `extends` / `implements`)
//! - `interface_declaration` → Interface
//! - `trait_declaration`     → Trait
//! - `enum_declaration`      → Enum
//! - `function_definition`   → Function (top-level)
//! - `method_declaration`    → Method (nested under class/trait)
//! - `namespace_definition`  → Namespace
//! - `property_declaration`  → Variable
//! - `const_declaration`     → Const
//! - `use_declaration`       → Import
//!
//! Call extraction handles:
//! - `function_call_expression` → bare-call
//! - `member_call_expression`   → `$obj->method()` (method call with receiver)
//! - `scoped_call_expression`   → `Class::method()` (static call with receiver)

use super::languages::SupportedLanguage;
use super::provider::{CallSite, ExtractedSymbol, LanguageProvider};
use crate::codegraph::types::NodeLabel;

pub struct PhpProvider;

impl LanguageProvider for PhpProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Php
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
        &["php", "phtml", "php3", "php4", "php5", "php8"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Class,
            NodeLabel::Interface,
            NodeLabel::Trait,
            NodeLabel::Enum,
            NodeLabel::Function,
            NodeLabel::Method,
            NodeLabel::Namespace,
            NodeLabel::Variable,
            NodeLabel::Const,
            NodeLabel::Import,
        ]
    }
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_php::LANGUAGE_PHP.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_php_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_php_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    match node.kind() {
        "namespace_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Namespace, &node));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_php_node(child, file_path, source, symbols, Some(&qn));
                    }
                    return;
                }
            }
        }
        "namespace_use_declaration" => {
            let raw = node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .trim_start_matches("use")
                .trim_start_matches("function")
                .trim_start_matches("const")
                .trim()
                .trim_end_matches(';')
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
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                let extends = node
                    .child_by_field_name("base_clause")
                    .map(|n| text(n, source).replace("extends", "").trim().to_string());
                let implements = collect_implements(node, source);
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
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_php_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Interface, &node));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_php_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "trait_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Trait, &node));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_php_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Enum, &node));
            }
        }
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, parent_name, &name, NodeLabel::Function, &node));
                }
            }
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, parent_name, &name, NodeLabel::Method, &node));
                }
            }
        }
        "property_declaration" => {
            // property_declaration contains property_element(s) with
            // variable_name children.
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                collect_property_names(child, file_path, source, symbols, parent_name);
            }
        }
        "const_declaration" => {
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if child.kind() == "const_element"
                    && let Some(n) = child.child_by_field_name("name")
                {
                    let name = text(n, source);
                    if !name.is_empty() {
                        symbols.push(sym(file_path, parent_name, &name, NodeLabel::Const, &child));
                    }
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_php_node(child, file_path, source, symbols, parent_name);
    }
}

#[cfg(feature = "codegraph")]
fn collect_property_names(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    if node.kind() == "property_element"
        && let Some(n) = node.child_by_field_name("name")
    {
        let raw = text(n, source);
        let name = raw.trim_start_matches('$').to_string();
        if !name.is_empty() {
            symbols.push(sym(file_path, parent_name, &name, NodeLabel::Variable, &node));
        }
        return;
    }
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        collect_property_names(child, file_path, source, symbols, parent_name);
    }
}

#[cfg(feature = "codegraph")]
fn collect_implements(class_node: tree_sitter::Node, source: &str) -> Vec<String> {
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

    let language: tree_sitter::Language = tree_sitter_php::LANGUAGE_PHP.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_php_calls(tree.root_node(), file_path, content, &mut calls, &mut Vec::new());
    calls
}

#[cfg(feature = "codegraph")]
fn walk_php_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "namespace_definition"
        | "class_declaration"
        | "interface_declaration"
        | "trait_declaration"
        | "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_php_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "function_definition" | "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_php_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "function_call_expression" => {
            if let Some(caller) = enclosing.last()
                && let Some(func) = node.child_by_field_name("function")
            {
                let name = text(func, source);
                if !name.is_empty() {
                    calls.push(CallSite {
                        caller_qualified_name: caller.clone(),
                        callee_name: name,
                        line: node.start_position().row as u32 + 1,
                        is_method_call: false,
                        receiver: None,
                    });
                }
            }
        }
        "member_call_expression" => {
            if let Some(caller) = enclosing.last() {
                let name = node
                    .child_by_field_name("name")
                    .map(|n| text(n, source))
                    .unwrap_or_default();
                let receiver = node
                    .child_by_field_name("object")
                    .map(|n| text(n, source));
                if !name.is_empty() {
                    calls.push(CallSite {
                        caller_qualified_name: caller.clone(),
                        callee_name: name,
                        line: node.start_position().row as u32 + 1,
                        is_method_call: true,
                        receiver,
                    });
                }
            }
        }
        "scoped_call_expression" => {
            if let Some(caller) = enclosing.last() {
                let name = node
                    .child_by_field_name("name")
                    .map(|n| text(n, source))
                    .unwrap_or_default();
                let receiver = node
                    .child_by_field_name("scope")
                    .map(|n| text(n, source));
                if !name.is_empty() {
                    calls.push(CallSite {
                        caller_qualified_name: caller.clone(),
                        callee_name: name,
                        line: node.start_position().row as u32 + 1,
                        is_method_call: true,
                        receiver,
                    });
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_php_calls(child, file_path, source, calls, enclosing);
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
            ("interface ", NodeLabel::Interface),
            ("trait ", NodeLabel::Trait),
            ("function ", NodeLabel::Function),
            ("namespace ", NodeLabel::Namespace),
        ];
        for (prefix, label) in patterns {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '\\')
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
