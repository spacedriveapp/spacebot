//! Java language provider (Wave 5).
//!
//! Extracts:
//! - `class_declaration`       → Class (with `extends` and `implements` lists)
//! - `interface_declaration`   → Interface
//! - `enum_declaration`        → Enum
//! - `method_declaration`      → Method (nested under the owning class/interface)
//! - `constructor_declaration` → Method (name = class name)
//! - `field_declaration`       → Variable
//! - `annotation` / `marker_annotation` → Decorator
//! - `package_declaration`     → Module
//! - `import_declaration`      → Import
//!
//! Call extraction walks `method_invocation` nodes, which always carry a
//! `name` field and optionally an `object` field (for `obj.method()` or
//! `ClassName.method()`).

use super::provider::{CallSite, ExtractedSymbol, LanguageProvider};
use super::languages::SupportedLanguage;
use crate::codegraph::types::NodeLabel;

pub struct JavaProvider;

impl LanguageProvider for JavaProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Java
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
        &["java"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Class,
            NodeLabel::Interface,
            NodeLabel::Enum,
            NodeLabel::Method,
            NodeLabel::Variable,
            NodeLabel::Decorator,
            NodeLabel::Import,
            NodeLabel::Module,
        ]
    }
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_java_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_java_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    match node.kind() {
        "package_declaration" => {
            if let Some(name_node) = node.child(1) {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, None, &name, NodeLabel::Module, &node));
                }
            }
        }
        "import_declaration" => {
            // `import foo.bar.Baz;` — strip the trailing `;` and the leading
            // `import` keyword. The scoped_identifier child carries the path.
            let raw = node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .trim_start_matches("import")
                .trim_end_matches(';')
                .trim();
            let path = raw.trim_start_matches("static").trim().to_string();
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
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qname = qname(file_path, parent_name, &name);

                let extends = node
                    .child_by_field_name("superclass")
                    .and_then(|n| n.child_by_field_name("name").or(Some(n)))
                    .map(|n| text(n, source).replace("extends ", "").trim().to_string());

                let implements = collect_implements(node, source);

                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname.clone(),
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
                        walk_java_node(child, file_path, source, symbols, Some(&qname));
                    }
                }
                return;
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qname = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Interface, &node));

                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_java_node(child, file_path, source, symbols, Some(&qname));
                    }
                }
                return;
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qname = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Enum, &node));

                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_java_node(child, file_path, source, symbols, Some(&qname));
                    }
                }
                return;
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
        "constructor_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, parent_name, &name, NodeLabel::Method, &node));
                }
            }
        }
        "field_declaration" => {
            // `field_declaration → variable_declarator → name`
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if child.kind() == "variable_declarator"
                    && let Some(name_node) = child.child_by_field_name("name")
                {
                    let name = text(name_node, source);
                    if !name.is_empty() {
                        symbols.push(sym(
                            file_path,
                            parent_name,
                            &name,
                            NodeLabel::Variable,
                            &child,
                        ));
                    }
                }
            }
        }
        "annotation" | "marker_annotation" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, parent_name, &name, NodeLabel::Decorator, &node));
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_java_node(child, file_path, source, symbols, parent_name);
    }
}

#[cfg(feature = "codegraph")]
fn collect_implements(class_node: tree_sitter::Node, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(interfaces) = class_node.child_by_field_name("interfaces") {
        let cursor = &mut interfaces.walk();
        for child in interfaces.children(cursor) {
            // child is either `type_list` (Tree-sitter 0.23) or individual types.
            if child.kind() == "type_list" {
                let inner_cursor = &mut child.walk();
                for t in child.children(inner_cursor) {
                    let name = text(t, source);
                    let clean = name.trim_matches(',').trim().to_string();
                    if !clean.is_empty() && clean != "," {
                        out.push(clean);
                    }
                }
            }
        }
    }
    out
}

#[cfg(feature = "codegraph")]
fn text(node: tree_sitter::Node, source: &str) -> String {
    node.utf8_text(source.as_bytes()).unwrap_or("").to_string()
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(file_path: &str, content: &str) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_java_calls(
        tree.root_node(),
        file_path,
        content,
        &mut calls,
        &mut Vec::new(),
    );
    calls
}

#[cfg(feature = "codegraph")]
fn walk_java_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class_declaration" | "interface_declaration" | "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let outer = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(outer);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_java_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "method_declaration" | "constructor_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_java_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "method_invocation" => {
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
                        is_method_call: receiver.is_some(),
                        receiver,
                    });
                }
            }
            // Fall through — walk children to capture nested calls.
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_java_calls(child, file_path, source, calls, enclosing);
    }
}

fn extract_fallback(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = i as u32 + 1;

        let patterns: &[(&str, NodeLabel)] = &[
            ("public class ", NodeLabel::Class),
            ("class ", NodeLabel::Class),
            ("public interface ", NodeLabel::Interface),
            ("interface ", NodeLabel::Interface),
            ("public enum ", NodeLabel::Enum),
            ("enum ", NodeLabel::Enum),
            ("package ", NodeLabel::Module),
        ];

        for (prefix, label) in patterns {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
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
