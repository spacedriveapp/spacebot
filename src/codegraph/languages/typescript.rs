//! TypeScript/JavaScript language provider.
//!
//! Extracts Class, Function, Method, Variable, Interface, Enum, Type,
//! Decorator, Namespace, TypeAlias, and Const nodes from TS/JS files
//! using tree-sitter.

use super::provider::{ExtractedSymbol, LanguageProvider};
use super::SupportedLanguage;
use crate::codegraph::types::NodeLabel;

/// Tree-sitter queries for TypeScript symbol extraction.
///
/// These S-expression queries match AST nodes that correspond to code
/// graph symbols. Each capture produces one ExtractedSymbol.
#[cfg(feature = "codegraph")]
const TS_QUERIES: &str = r#"
; Classes
(class_declaration
  name: (type_identifier) @class.name) @class.def

; Functions
(function_declaration
  name: (identifier) @function.name) @function.def

; Arrow functions assigned to const/let/var
(lexical_declaration
  (variable_declarator
    name: (identifier) @function.name
    value: (arrow_function))) @function.def

; Methods
(method_definition
  name: (property_identifier) @method.name) @method.def

; Interfaces
(interface_declaration
  name: (type_identifier) @interface.name) @interface.def

; Enums
(enum_declaration
  name: (identifier) @enum.name) @enum.def

; Type aliases
(type_alias_declaration
  name: (type_identifier) @type.name) @type.def

; Exports
(export_statement
  declaration: (_) @export.decl)

; Imports
(import_statement
  source: (string) @import.source) @import.def
"#;

pub struct TypeScriptProvider;

impl LanguageProvider for TypeScriptProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::TypeScript
    }

    fn extract_symbols(&self, file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
        #[cfg(feature = "codegraph")]
        {
            extract_with_tree_sitter(file_path, content, true)
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
            NodeLabel::Interface,
            NodeLabel::Enum,
            NodeLabel::Decorator,
            NodeLabel::Import,
            NodeLabel::Type,
            NodeLabel::Namespace,
            NodeLabel::TypeAlias,
            NodeLabel::Const,
        ]
    }
}

pub struct JavaScriptProvider;

impl LanguageProvider for JavaScriptProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::JavaScript
    }

    fn extract_symbols(&self, file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
        #[cfg(feature = "codegraph")]
        {
            extract_with_tree_sitter(file_path, content, false)
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

/// Extract symbols using tree-sitter with the TypeScript or JavaScript grammar.
#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(
    file_path: &str,
    content: &str,
    is_typescript: bool,
) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language = if is_typescript {
        if file_path.ends_with(".tsx") {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    } else {
        tree_sitter_javascript::LANGUAGE.into()
    };

    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        tracing::warn!(file = file_path, "failed to set tree-sitter language");
        return extract_fallback(file_path, content);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => {
            tracing::warn!(file = file_path, "tree-sitter parse returned None");
            return extract_fallback(file_path, content);
        }
    };

    let mut symbols = Vec::new();
    let root = tree.root_node();

    // Walk the tree manually for reliability across grammar versions.
    walk_ts_node(root, file_path, content, &mut symbols, None);

    symbols
}

#[cfg(feature = "codegraph")]
fn walk_ts_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    let kind = node.kind();

    match kind {
        "class_declaration" | "class" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let qname = qualified_name(file_path, parent_name, &name);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname.clone(),
                    label: NodeLabel::Class,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends: child_text_by_field(node, "superclass", source),
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
                // Recurse into class body with class as parent.
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_children(body, file_path, source, symbols, Some(&qname));
                }
                return; // Don't recurse into children again.
            }
        }
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qualified_name(file_path, parent_name, &name),
                    label: NodeLabel::Function,
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
        "method_definition" | "public_field_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let label = if kind == "method_definition" {
                    NodeLabel::Method
                } else {
                    NodeLabel::Variable
                };
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qualified_name(file_path, parent_name, &name),
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
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qualified_name(file_path, parent_name, &name),
                    label: NodeLabel::Interface,
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
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qualified_name(file_path, parent_name, &name),
                    label: NodeLabel::Enum,
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
        "type_alias_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qualified_name(file_path, parent_name, &name),
                    label: NodeLabel::TypeAlias,
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
        "import_statement" => {
            if let Some(source_node) = node.child_by_field_name("source") {
                let source_text = node_text(source_node, source)
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                symbols.push(ExtractedSymbol {
                    name: source_text.clone(),
                    qualified_name: format!("{file_path}::import::{source_text}"),
                    label: NodeLabel::Import,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: None,
                    import_source: Some(source_text),
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
            }
        }
        "lexical_declaration" => {
            // Handle `const foo = () => {}` (arrow functions assigned to variables).
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if child.kind() == "variable_declarator" {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        if let Some(value_node) = child.child_by_field_name("value") {
                            let label = if value_node.kind() == "arrow_function"
                                || value_node.kind() == "function"
                            {
                                NodeLabel::Function
                            } else {
                                NodeLabel::Variable
                            };
                            let name = node_text(name_node, source);
                            symbols.push(ExtractedSymbol {
                                name: name.clone(),
                                qualified_name: qualified_name(file_path, parent_name, &name),
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
                }
            }
        }
        _ => {}
    }

    // Recurse into children.
    walk_ts_children(node, file_path, source, symbols, parent_name);
}

#[cfg(feature = "codegraph")]
fn walk_ts_children(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_ts_node(child, file_path, source, symbols, parent_name);
    }
}

#[cfg(feature = "codegraph")]
fn node_text(node: tree_sitter::Node, source: &str) -> String {
    node.utf8_text(source.as_bytes())
        .unwrap_or("")
        .to_string()
}

#[cfg(feature = "codegraph")]
fn child_text_by_field(
    node: tree_sitter::Node,
    field: &str,
    source: &str,
) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| node_text(n, source))
}

/// Fallback extractor when tree-sitter is not available (no `codegraph` feature).
fn extract_fallback(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = i as u32 + 1;

        if let Some(name) = extract_simple_pattern(trimmed, "function ") {
            symbols.push(simple_symbol(file_path, &name, NodeLabel::Function, line_num));
        } else if let Some(name) = extract_simple_pattern(trimmed, "class ") {
            symbols.push(simple_symbol(file_path, &name, NodeLabel::Class, line_num));
        } else if let Some(name) = extract_simple_pattern(trimmed, "interface ") {
            symbols.push(simple_symbol(file_path, &name, NodeLabel::Interface, line_num));
        } else if let Some(name) = extract_simple_pattern(trimmed, "enum ") {
            symbols.push(simple_symbol(file_path, &name, NodeLabel::Enum, line_num));
        } else if let Some(name) = extract_simple_pattern(trimmed, "type ") {
            if name.contains('=') || name.contains('<') {
                let clean = name.split(|c: char| c == '=' || c == '<').next().unwrap_or(&name).trim();
                symbols.push(simple_symbol(file_path, clean, NodeLabel::TypeAlias, line_num));
            }
        }
    }
    symbols
}

fn extract_simple_pattern(line: &str, prefix: &str) -> Option<String> {
    if !line.starts_with(prefix) && !line.starts_with(&format!("export {prefix}")) {
        return None;
    }
    let after = if line.starts_with("export ") {
        &line[("export ".len() + prefix.len())..]
    } else {
        &line[prefix.len()..]
    };
    let name: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn qualified_name(file_path: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}::{name}"),
        None => format!("{file_path}::{name}"),
    }
}

fn simple_symbol(
    file_path: &str,
    name: &str,
    label: NodeLabel,
    line: u32,
) -> ExtractedSymbol {
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
