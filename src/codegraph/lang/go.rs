//! Go language provider (Wave 5).
//!
//! Extracts:
//! - `function_declaration`  → Function
//! - `method_declaration`    → Method (with receiver-based parent qname)
//! - `type_declaration → type_spec + struct_type`    → Struct
//! - `type_declaration → type_spec + interface_type` → Interface
//! - `const_declaration`     → Const
//! - `import_declaration`    → Import (one per spec)
//! - `package_clause`        → Module
//!
//! Call extraction walks `call_expression`s nested inside the enclosing
//! function or method, handling both bare identifier calls and
//! `selector_expression` calls (`pkg.Func()` or `receiver.Method()`).

use super::provider::{CallSite, ExtractedSymbol, LanguageProvider};
use super::languages::SupportedLanguage;
use crate::codegraph::types::NodeLabel;

pub struct GoProvider;

impl LanguageProvider for GoProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Go
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
        &["go"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Function,
            NodeLabel::Method,
            NodeLabel::Struct,
            NodeLabel::Interface,
            NodeLabel::Const,
            NodeLabel::Variable,
            NodeLabel::Import,
            NodeLabel::Module,
        ]
    }
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_go_node(tree.root_node(), file_path, content, &mut symbols);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_go_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    match node.kind() {
        "package_clause" => {
            // (package_clause (package_identifier))
            if let Some(name_node) = node.child(1) {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, None, &name, NodeLabel::Module, &node));
                }
            }
        }
        "import_declaration" => {
            // (import_declaration (import_spec_list (import_spec ...))) or
            // (import_declaration (import_spec ...))
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                collect_import_specs(child, file_path, source, symbols);
            }
        }
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, None, &name, NodeLabel::Function, &node));
                }
            }
        }
        "method_declaration" => {
            // Resolve receiver type for the parent qname. The receiver is
            // `(receiver parameter_list (parameter_declaration ... type))`.
            let receiver_type = node
                .child_by_field_name("receiver")
                .and_then(|recv| receiver_type_name(recv, source));
            let parent_qname = receiver_type
                .as_deref()
                .map(|rt| format!("{file_path}::{rt}"));

            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(
                        file_path,
                        parent_qname.as_deref(),
                        &name,
                        NodeLabel::Method,
                        &node,
                    ));
                }
            }
        }
        "type_declaration" => {
            // A `type` declaration wraps one or more type_specs.
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if child.kind() == "type_spec" || child.kind() == "type_alias" {
                    walk_type_spec(child, file_path, source, symbols);
                }
            }
            return;
        }
        "const_declaration" | "var_declaration" => {
            // const ( Foo = 1; Bar = 2 ) or var Foo int
            let cursor = &mut node.walk();
            let is_const = node.kind() == "const_declaration";
            for child in node.children(cursor) {
                if child.kind() == "const_spec" || child.kind() == "var_spec" {
                    let name_cursor = &mut child.walk();
                    for n in child.children(name_cursor) {
                        if n.kind() == "identifier" {
                            let name = text(n, source);
                            if !name.is_empty() {
                                let label = if is_const {
                                    NodeLabel::Const
                                } else {
                                    NodeLabel::Variable
                                };
                                symbols.push(sym(file_path, None, &name, label, &child));
                                break;
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_go_node(child, file_path, source, symbols);
    }
}

#[cfg(feature = "codegraph")]
fn walk_type_spec(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => text(n, source),
        None => return,
    };
    if name.is_empty() {
        return;
    }

    let type_node = match node.child_by_field_name("type") {
        Some(t) => t,
        None => {
            symbols.push(sym(file_path, None, &name, NodeLabel::TypeAlias, &node));
            return;
        }
    };

    match type_node.kind() {
        "struct_type" => {
            let parent_qname = format!("{file_path}::{name}");
            symbols.push(sym(file_path, None, &name, NodeLabel::Struct, &node));

            // Extract field declarations as Variables under the struct.
            if let Some(fields) = type_node.child_by_field_name("fields")
                .or_else(|| type_node.child(0))
            {
                let cursor = &mut fields.walk();
                for child in fields.children(cursor) {
                    if child.kind() == "field_declaration" {
                        let field_cursor = &mut child.walk();
                        for f in child.children(field_cursor) {
                            if f.kind() == "field_identifier" {
                                let fname = text(f, source);
                                if !fname.is_empty() {
                                    symbols.push(sym(
                                        file_path,
                                        Some(&parent_qname),
                                        &fname,
                                        NodeLabel::Variable,
                                        &child,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        "interface_type" => {
            let parent_qname = format!("{file_path}::{name}");
            symbols.push(sym(file_path, None, &name, NodeLabel::Interface, &node));

            // Extract interface methods.
            let cursor = &mut type_node.walk();
            for child in type_node.children(cursor) {
                if (child.kind() == "method_elem" || child.kind() == "method_spec")
                    && let Some(mn) = child.child_by_field_name("name")
                {
                    let mname = text(mn, source);
                    if !mname.is_empty() {
                        symbols.push(sym(
                            file_path,
                            Some(&parent_qname),
                            &mname,
                            NodeLabel::Method,
                            &child,
                        ));
                    }
                }
            }
        }
        _ => {
            symbols.push(sym(file_path, None, &name, NodeLabel::TypeAlias, &node));
        }
    }
}

#[cfg(feature = "codegraph")]
fn receiver_type_name(recv_node: tree_sitter::Node, source: &str) -> Option<String> {
    // receiver is a parameter_list containing a parameter_declaration whose
    // `type` field is either a type_identifier or a pointer_type wrapping one.
    let cursor = &mut recv_node.walk();
    for child in recv_node.children(cursor) {
        if child.kind() == "parameter_declaration"
            && let Some(type_node) = child.child_by_field_name("type")
        {
            return extract_type_ident(type_node, source);
        }
    }
    None
}

#[cfg(feature = "codegraph")]
fn extract_type_ident(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" => Some(text(node, source)),
        "pointer_type" => node.child(1).and_then(|n| extract_type_ident(n, source)),
        "qualified_type" => node.child_by_field_name("name").map(|n| text(n, source)),
        _ => None,
    }
}

#[cfg(feature = "codegraph")]
fn collect_import_specs(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    match node.kind() {
        "import_spec" => {
            // path is an interpreted_string_literal; strip the quotes.
            if let Some(path_node) = node.child_by_field_name("path") {
                let raw = text(path_node, source);
                let path = raw.trim_matches('"').to_string();
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
        "import_spec_list" => {
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                collect_import_specs(child, file_path, source, symbols);
            }
        }
        _ => {}
    }
}

#[cfg(feature = "codegraph")]
fn text(node: tree_sitter::Node, source: &str) -> String {
    node.utf8_text(source.as_bytes()).unwrap_or("").to_string()
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(file_path: &str, content: &str) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_go_calls(
        tree.root_node(),
        file_path,
        content,
        &mut calls,
        &mut Vec::new(),
    );
    calls
}

#[cfg(feature = "codegraph")]
fn walk_go_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let fq = format!("{file_path}::{name}");
                enclosing.push(fq);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_go_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "method_declaration" => {
            let receiver_type = node
                .child_by_field_name("receiver")
                .and_then(|r| receiver_type_name(r, source));

            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let fq = match receiver_type {
                    Some(rt) => format!("{file_path}::{rt}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_go_calls(child, file_path, source, calls, enclosing);
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
                    "selector_expression" => {
                        // pkg.Func() or recv.Method()
                        let name = func_node
                            .child_by_field_name("field")
                            .map(|n| text(n, source))
                            .unwrap_or_default();
                        let receiver = func_node
                            .child_by_field_name("operand")
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
                    _ => {}
                }
            }
            // Fall through so we also capture nested calls inside the args.
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_go_calls(child, file_path, source, calls, enclosing);
    }
}

fn extract_fallback(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = i as u32 + 1;

        let patterns: &[(&str, NodeLabel)] = &[
            ("func ", NodeLabel::Function),
            ("type ", NodeLabel::Struct),
            ("package ", NodeLabel::Module),
        ];

        for (prefix, label) in patterns {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                // `func (r *T) Name(...)` → Method, not Function.
                if *prefix == "func " && rest.starts_with('(') {
                    if let Some((_, method_part)) = rest.split_once(')') {
                        let name: String = method_part
                            .trim_start()
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect();
                        if !name.is_empty() {
                            symbols.push(fallback_sym(file_path, &name, NodeLabel::Method, line_num));
                        }
                    }
                    break;
                }
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
fn sym(
    file_path: &str,
    parent: Option<&str>,
    name: &str,
    label: NodeLabel,
    node: &tree_sitter::Node,
) -> ExtractedSymbol {
    ExtractedSymbol {
        name: name.to_string(),
        qualified_name: match parent {
            Some(p) => format!("{p}::{name}"),
            None => format!("{file_path}::{name}"),
        },
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
