//! Python language provider.

use super::provider::{ExtractedSymbol, LanguageProvider};
use super::SupportedLanguage;
use crate::codegraph::types::NodeLabel;

pub struct PythonProvider;

impl LanguageProvider for PythonProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Python
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
            NodeLabel::Class,
            NodeLabel::Function,
            NodeLabel::Method,
            NodeLabel::Variable,
            NodeLabel::Decorator,
            NodeLabel::Import,
        ]
    }
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_py_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_py_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    let kind = node.kind();
    match kind {
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                let qname = qname(file_path, parent_name, &name);
                let extends = node
                    .child_by_field_name("superclasses")
                    .and_then(|n| n.child(0))
                    .map(|n| n.utf8_text(source.as_bytes()).unwrap_or("").to_string());
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname.clone(),
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
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_py_node(child, file_path, source, symbols, Some(&qname));
                    }
                }
                return;
            }
        }
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                let label = if parent_name.is_some() {
                    NodeLabel::Method
                } else {
                    NodeLabel::Function
                };
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname(file_path, parent_name, &name),
                    label,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
            }
        }
        "import_statement" | "import_from_statement" => {
            let text = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            let source_module = if kind == "import_from_statement" {
                node.child_by_field_name("module_name")
                    .map(|n| n.utf8_text(source.as_bytes()).unwrap_or("").to_string())
            } else {
                Some(text.trim_start_matches("import ").trim().to_string())
            };
            symbols.push(ExtractedSymbol {
                name: source_module.clone().unwrap_or_default(),
                qualified_name: format!("{file_path}::import::{}", source_module.as_deref().unwrap_or("?")),
                label: NodeLabel::Import,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                parent: None,
                import_source: source_module,
                extends: None,
                implements: Vec::new(),
                decorates: None,
                metadata: std::collections::HashMap::new(),
            });
        }
        "decorated_definition" => {
            // Process the decorators, then recurse into the definition.
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if child.kind() == "decorator" {
                    let dec_text = child.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                    symbols.push(ExtractedSymbol {
                        name: dec_text.trim_start_matches('@').to_string(),
                        qualified_name: format!("{file_path}::decorator::{dec_text}"),
                        label: NodeLabel::Decorator,
                        line_start: child.start_position().row as u32 + 1,
                        line_end: child.end_position().row as u32 + 1,
                        parent: parent_name.map(String::from),
                        import_source: None,
                        extends: None,
                        implements: Vec::new(),
                        decorates: None,
                        metadata: std::collections::HashMap::new(),
                    });
                } else {
                    walk_py_node(child, file_path, source, symbols, parent_name);
                }
            }
            return;
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_py_node(child, file_path, source, symbols, parent_name);
    }
}

fn extract_fallback(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = i as u32 + 1;
        if let Some(rest) = trimmed.strip_prefix("class ") {
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                symbols.push(sym(file_path, &name, NodeLabel::Class, line_num));
            }
        } else if let Some(rest) = trimmed.strip_prefix("def ") {
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                symbols.push(sym(file_path, &name, NodeLabel::Function, line_num));
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

fn sym(file_path: &str, name: &str, label: NodeLabel, line: u32) -> ExtractedSymbol {
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
