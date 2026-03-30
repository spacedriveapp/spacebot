//! Rust language provider.

use super::provider::{ExtractedSymbol, LanguageProvider};
use super::SupportedLanguage;
use crate::codegraph::types::NodeLabel;

pub struct RustProvider;

impl LanguageProvider for RustProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Rust
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

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Struct,
            NodeLabel::Enum,
            NodeLabel::Trait,
            NodeLabel::Impl,
            NodeLabel::Function,
            NodeLabel::Method,
            NodeLabel::Macro,
            NodeLabel::TypeAlias,
            NodeLabel::Const,
            NodeLabel::Import,
        ]
    }
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_rust_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_rust_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    let kind = node.kind();
    match kind {
        "struct_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Struct, &node));
            }
        }
        "enum_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Enum, &node));
            }
        }
        "trait_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qname = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Trait, &node));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_rust_node(child, file_path, source, symbols, Some(&qname));
                    }
                }
                return;
            }
        }
        "impl_item" => {
            // Get the type being impl'd.
            let impl_name = node
                .child_by_field_name("type")
                .map(|n| text(n, source))
                .unwrap_or_else(|| "impl".to_string());
            let qname = qname(file_path, parent_name, &impl_name);
            symbols.push(ExtractedSymbol {
                name: impl_name.clone(),
                qualified_name: qname.clone(),
                label: NodeLabel::Impl,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                parent: parent_name.map(String::from),
                import_source: None,
                extends: node.child_by_field_name("trait").map(|n| text(n, source)),
                implements: Vec::new(),
                decorates: None,
                metadata: std::collections::HashMap::new(),
            });
            if let Some(body) = node.child_by_field_name("body") {
                let cursor = &mut body.walk();
                for child in body.children(cursor) {
                    walk_rust_node(child, file_path, source, symbols, Some(&qname));
                }
            }
            return;
        }
        "function_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let label = if parent_name.is_some() {
                    NodeLabel::Method
                } else {
                    NodeLabel::Function
                };
                symbols.push(sym(file_path, parent_name, &name, label, &node));
            }
        }
        "macro_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Macro, &node));
            }
        }
        "type_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::TypeAlias, &node));
            }
        }
        "const_item" | "static_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Const, &node));
            }
        }
        "use_declaration" => {
            let use_text = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            symbols.push(ExtractedSymbol {
                name: use_text.clone(),
                qualified_name: format!("{file_path}::use::{use_text}"),
                label: NodeLabel::Import,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                parent: None,
                import_source: Some(use_text),
                extends: None,
                implements: Vec::new(),
                decorates: None,
                metadata: std::collections::HashMap::new(),
            });
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_rust_node(child, file_path, source, symbols, parent_name);
    }
}

#[cfg(feature = "codegraph")]
fn text(node: tree_sitter::Node, source: &str) -> String {
    node.utf8_text(source.as_bytes()).unwrap_or("").to_string()
}

fn extract_fallback(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = i as u32 + 1;
        let patterns: &[(&str, NodeLabel)] = &[
            ("pub struct ", NodeLabel::Struct),
            ("struct ", NodeLabel::Struct),
            ("pub enum ", NodeLabel::Enum),
            ("enum ", NodeLabel::Enum),
            ("pub trait ", NodeLabel::Trait),
            ("trait ", NodeLabel::Trait),
            ("pub fn ", NodeLabel::Function),
            ("fn ", NodeLabel::Function),
            ("pub async fn ", NodeLabel::Function),
            ("async fn ", NodeLabel::Function),
            ("macro_rules! ", NodeLabel::Macro),
        ];
        for (prefix, label) in patterns {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if !name.is_empty() {
                    symbols.push(fallback_sym(file_path, &name, *label, line_num));
                    break;
                }
            }
        }
    }
    symbols
}

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
