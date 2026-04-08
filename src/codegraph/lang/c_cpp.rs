//! C and C++ language providers (Wave 5).
//!
//! Both providers share `walk_c_node` / `walk_c_calls`; the C grammar
//! simply never produces C++-only kinds like `class_specifier` or
//! `namespace_definition`, so a single walker handles both cleanly.
//!
//! Extracts:
//! - `function_definition` (and prototype `declaration`) → Function
//! - `struct_specifier`, `union_specifier`               → Struct
//! - `class_specifier` (C++)                             → Class
//! - `enum_specifier`                                    → Enum
//! - `namespace_definition` (C++)                        → Namespace
//! - `preproc_include`                                   → Import
//! - `type_definition`                                   → TypeAlias

use super::provider::{CallSite, ExtractedSymbol, LanguageProvider};
use super::languages::SupportedLanguage;
use crate::codegraph::types::NodeLabel;

pub struct CProvider;
pub struct CppProvider;

impl LanguageProvider for CProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::C
    }

    fn extract_symbols(&self, file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
        #[cfg(feature = "codegraph")]
        {
            extract_with_tree_sitter(file_path, content, Dialect::C)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            extract_fallback(file_path, content)
        }
    }

    fn extract_calls(&self, file_path: &str, content: &str) -> Vec<CallSite> {
        #[cfg(feature = "codegraph")]
        {
            extract_calls_tree_sitter(file_path, content, Dialect::C)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            let _ = (file_path, content);
            Vec::new()
        }
    }

    fn file_extensions(&self) -> &[&str] {
        &["c", "h"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Function,
            NodeLabel::Struct,
            NodeLabel::Enum,
            NodeLabel::TypeAlias,
            NodeLabel::Variable,
            NodeLabel::Import,
        ]
    }
}

impl LanguageProvider for CppProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Cpp
    }

    fn extract_symbols(&self, file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
        #[cfg(feature = "codegraph")]
        {
            extract_with_tree_sitter(file_path, content, Dialect::Cpp)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            extract_fallback(file_path, content)
        }
    }

    fn extract_calls(&self, file_path: &str, content: &str) -> Vec<CallSite> {
        #[cfg(feature = "codegraph")]
        {
            extract_calls_tree_sitter(file_path, content, Dialect::Cpp)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            let _ = (file_path, content);
            Vec::new()
        }
    }

    fn file_extensions(&self) -> &[&str] {
        &["cpp", "cc", "cxx", "hpp", "hxx", "hh"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Function,
            NodeLabel::Method,
            NodeLabel::Class,
            NodeLabel::Struct,
            NodeLabel::Enum,
            NodeLabel::Namespace,
            NodeLabel::TypeAlias,
            NodeLabel::Variable,
            NodeLabel::Import,
        ]
    }
}

#[cfg(feature = "codegraph")]
#[derive(Copy, Clone)]
enum Dialect {
    C,
    Cpp,
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(
    file_path: &str,
    content: &str,
    dialect: Dialect,
) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = match dialect {
        Dialect::C => tree_sitter_c::LANGUAGE.into(),
        Dialect::Cpp => tree_sitter_cpp::LANGUAGE.into(),
    };

    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_c_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_c_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    match node.kind() {
        "preproc_include" => {
            // #include "foo.h" or #include <foo.h>. Child path is either
            // `string_literal` or `system_lib_string`.
            if let Some(path_node) = node.child_by_field_name("path") {
                let raw = text(path_node, source);
                let path = raw.trim_matches(|c| c == '"' || c == '<' || c == '>').to_string();
                if !path.is_empty() {
                    symbols.push(ExtractedSymbol {
                        name: path.clone(),
                        qualified_name: format!("{file_path}::include::{path}"),
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
        "namespace_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Namespace, &node));

                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_c_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "class_specifier" | "struct_specifier" | "union_specifier" => {
            let label = match node.kind() {
                "class_specifier" => NodeLabel::Class,
                _ => NodeLabel::Struct,
            };
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if name.is_empty() {
                    // Anonymous struct/union — skip emitting but still walk.
                } else {
                    let qn = qname(file_path, parent_name, &name);
                    symbols.push(sym(file_path, parent_name, &name, label, &node));

                    if let Some(body) = node.child_by_field_name("body") {
                        let cursor = &mut body.walk();
                        for child in body.children(cursor) {
                            walk_c_node(child, file_path, source, symbols, Some(&qn));
                        }
                    }
                    return;
                }
            }
        }
        "enum_specifier" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, parent_name, &name, NodeLabel::Enum, &node));
                }
            }
        }
        "function_definition" => {
            if let Some(declarator) = node.child_by_field_name("declarator")
                && let Some(name) = extract_function_name(declarator, source)
            {
                let label = if parent_name.is_some() {
                    NodeLabel::Method
                } else {
                    NodeLabel::Function
                };
                symbols.push(sym(file_path, parent_name, &name, label, &node));
            }
        }
        "declaration" => {
            // Function prototypes look like `declaration → function_declarator`.
            if let Some(declarator) = node.child_by_field_name("declarator")
                && declarator.kind() == "function_declarator"
                && let Some(name) = extract_function_name(declarator, source)
            {
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Function, &node));
            }
        }
        "type_definition" => {
            // `typedef struct { ... } Foo;` or `typedef int Foo;`
            if let Some(declarator) = node.child_by_field_name("declarator") {
                let name = text(declarator, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, parent_name, &name, NodeLabel::TypeAlias, &node));
                }
            }
        }
        "field_declaration" => {
            // Struct / class member variable.
            if let Some(declarator) = node.child_by_field_name("declarator")
                && let Some(name) = extract_plain_name(declarator, source)
            {
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Variable, &node));
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_c_node(child, file_path, source, symbols, parent_name);
    }
}

/// Walk a `function_declarator` (which may be wrapped in pointer/reference
/// layers) down to the base identifier — or `qualified_identifier` for
/// out-of-line method definitions like `void Foo::bar()`.
#[cfg(feature = "codegraph")]
fn extract_function_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "function_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|n| extract_function_name(n, source)),
        "pointer_declarator" | "reference_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|n| extract_function_name(n, source)),
        "identifier" | "field_identifier" => Some(text(node, source)),
        "qualified_identifier" => {
            // `Foo::bar` — return only the leaf name; the Class parent qname
            // already exists from walking the class body, and out-of-line
            // method bodies are rare enough to accept the slight coverage gap.
            node.child_by_field_name("name")
                .map(|n| text(n, source))
        }
        "destructor_name" | "operator_name" => Some(text(node, source)),
        _ => None,
    }
}

#[cfg(feature = "codegraph")]
fn extract_plain_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(text(node, source)),
        "array_declarator" | "pointer_declarator" | "reference_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|n| extract_plain_name(n, source)),
        _ => None,
    }
}

#[cfg(feature = "codegraph")]
fn text(node: tree_sitter::Node, source: &str) -> String {
    node.utf8_text(source.as_bytes()).unwrap_or("").to_string()
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(
    file_path: &str,
    content: &str,
    dialect: Dialect,
) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = match dialect {
        Dialect::C => tree_sitter_c::LANGUAGE.into(),
        Dialect::Cpp => tree_sitter_cpp::LANGUAGE.into(),
    };

    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_c_calls(
        tree.root_node(),
        file_path,
        content,
        &mut calls,
        &mut Vec::new(),
    );
    calls
}

#[cfg(feature = "codegraph")]
fn walk_c_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "namespace_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let outer = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(outer);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_c_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "class_specifier" | "struct_specifier" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    let outer = match enclosing.last() {
                        Some(p) => format!("{p}::{name}"),
                        None => format!("{file_path}::{name}"),
                    };
                    enclosing.push(outer);
                    let cursor = &mut node.walk();
                    for child in node.children(cursor) {
                        walk_c_calls(child, file_path, source, calls, enclosing);
                    }
                    enclosing.pop();
                    return;
                }
            }
        }
        "function_definition" => {
            if let Some(declarator) = node.child_by_field_name("declarator")
                && let Some(name) = extract_function_name(declarator, source)
            {
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_c_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "call_expression" => {
            if let Some(caller) = enclosing.last()
                && let Some(func_node) = node.child_by_field_name("function")
            {
                match func_node.kind() {
                    "identifier" => {
                        let name = text(func_node, source);
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
                    "field_expression" => {
                        // obj.method() or obj->method()
                        let name = func_node
                            .child_by_field_name("field")
                            .map(|n| text(n, source))
                            .unwrap_or_default();
                        let receiver = func_node
                            .child_by_field_name("argument")
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
                    "qualified_identifier" => {
                        // Ns::func() or Class::method()
                        let name = func_node
                            .child_by_field_name("name")
                            .map(|n| text(n, source))
                            .unwrap_or_default();
                        let receiver = func_node
                            .child_by_field_name("scope")
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
                    _ => {}
                }
            }
            // Fall through for nested calls.
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_c_calls(child, file_path, source, calls, enclosing);
    }
}

fn extract_fallback(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = i as u32 + 1;

        let patterns: &[(&str, NodeLabel)] = &[
            ("struct ", NodeLabel::Struct),
            ("class ", NodeLabel::Class),
            ("namespace ", NodeLabel::Namespace),
            ("typedef ", NodeLabel::TypeAlias),
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
