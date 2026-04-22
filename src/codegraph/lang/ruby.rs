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
use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider};
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

    fn extract_accesses(&self, file_path: &str, content: &str) -> Vec<AccessSite> {
        #[cfg(feature = "codegraph")]
        {
            extract_accesses_tree_sitter(file_path, content)
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
            NodeLabel::Variable,
            NodeLabel::Import,
        ]
    }

    fn queries(&self) -> Option<&'static super::queries::QuerySet> {
        Some(&super::queries::ruby::QUERY_SET)
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
                    ..Default::default()
                });

                // Ruby has no field declarations — instance variables (`@x`)
                // are discovered by use. Walk the class body collecting all
                // distinct `@x` names and emit one Variable node per name
                // parented to the class.
                let mut fields: std::collections::HashMap<String, (u32, u32)> =
                    std::collections::HashMap::new();
                collect_ruby_instance_vars(node, source, &mut fields);
                for (fname, (line_start, line_end)) in &fields {
                    symbols.push(ExtractedSymbol {
                        name: fname.clone(),
                        qualified_name: format!("{qn}::{fname}"),
                        label: NodeLabel::Variable,
                        line_start: *line_start,
                        line_end: *line_end,
                        parent: Some(qn.clone()),
                        import_source: None,
                        extends: None,
                        implements: Vec::new(),
                        decorates: None,
                        metadata: std::collections::HashMap::new(),
                        ..Default::default()
                    });
                }

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
                    let method_qname = qname(file_path, parent_name, &name);
                    symbols.push(sym(file_path, parent_name, &name, label, &node));
                    if let Some(params) = node.child_by_field_name("parameters") {
                        collect_ruby_params(params, source, &method_qname, symbols);
                    }
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
                            ..Default::default()
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

/// Collect Ruby method/singleton_method parameters as Parameter symbols.
/// Tree-sitter-ruby wraps params in a `method_parameters` node containing:
/// - `identifier` for plain `x`
/// - `optional_parameter` (`x = 5`) — name lives in `name` field
/// - `keyword_parameter` (`x:`) — name in `name` field
/// - `splat_parameter` (`*args`) — first identifier child
/// - `hash_splat_parameter` (`**kwargs`) — first identifier child
/// - `block_parameter` (`&block`) — first identifier child
///
/// Every identifier that lands here is a real positional argument, so
/// there's no self/this to skip the way Python has `self`/`cls`.
#[cfg(feature = "codegraph")]
fn collect_ruby_params(
    params_node: tree_sitter::Node,
    source: &str,
    method_qname: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let cursor = &mut params_node.walk();
    for child in params_node.children(cursor) {
        let pname = match child.kind() {
            "identifier" => Some(text(child, source)),
            "optional_parameter" | "keyword_parameter" => child
                .child_by_field_name("name")
                .map(|n| text(n, source))
                .or_else(|| first_identifier_child(child, source)),
            "splat_parameter" | "hash_splat_parameter" | "block_parameter" => {
                first_identifier_child(child, source)
            }
            _ => None,
        };

        let Some(pname) = pname else { continue };
        if pname.is_empty() {
            continue;
        }
        symbols.push(ExtractedSymbol {
            name: pname.clone(),
            qualified_name: format!("{method_qname}::{pname}"),
            label: NodeLabel::Parameter,
            line_start: child.start_position().row as u32 + 1,
            line_end: child.end_position().row as u32 + 1,
            parent: Some(method_qname.to_string()),
            import_source: None,
            extends: None,
            implements: Vec::new(),
            decorates: None,
            metadata: std::collections::HashMap::new(),
            ..Default::default()
        });
    }
}

#[cfg(feature = "codegraph")]
fn first_identifier_child(node: tree_sitter::Node, source: &str) -> Option<String> {
    let cursor = &mut node.walk();
    for c in node.children(cursor) {
        if c.kind() == "identifier" {
            return Some(text(c, source));
        }
    }
    None
}

/// Collect all distinct `@instance_var` names referenced anywhere inside
/// a class body. Used to synthesize Ruby field nodes since Ruby has no
/// declarative field syntax. Skips nested class bodies.
#[cfg(feature = "codegraph")]
fn collect_ruby_instance_vars(
    node: tree_sitter::Node,
    source: &str,
    fields: &mut std::collections::HashMap<String, (u32, u32)>,
) {
    let kind = node.kind();
    if kind == "class" {
        // Skip the outer entry's own children handled by the caller.
        // For nested classes encountered during recursion, return so
        // their @vars belong to their own class.
        // We'll recurse manually below for the top-level call so this
        // only affects deeper nesting.
    }
    if kind == "instance_variable" {
        let raw = text(node, source);
        // Strip the leading `@` (or `@@` for class variables which we
        // also surface as fields for graph purposes).
        let fname = raw.trim_start_matches('@').to_string();
        if !fname.is_empty() {
            fields.entry(fname).or_insert((
                node.start_position().row as u32 + 1,
                node.end_position().row as u32 + 1,
            ));
        }
    } else if kind == "class_variable" {
        // `@@count` style — also a field on the class.
        let raw = text(node, source);
        let fname = raw.trim_start_matches('@').to_string();
        if !fname.is_empty() {
            fields.entry(fname).or_insert((
                node.start_position().row as u32 + 1,
                node.end_position().row as u32 + 1,
            ));
        }
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        // Don't descend into nested class definitions — their fields
        // belong to the inner class, which gets its own collect pass.
        if child.kind() == "class" {
            continue;
        }
        collect_ruby_instance_vars(child, source, fields);
    }
}

#[cfg(feature = "codegraph")]
fn extract_accesses_tree_sitter(file_path: &str, content: &str) -> Vec<AccessSite> {
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

    let mut accesses = Vec::new();
    walk_ruby_accesses(
        tree.root_node(),
        file_path,
        content,
        &mut accesses,
        &mut Vec::new(),
    );
    accesses
}

/// Walk the Ruby AST collecting `@x` instance variable references inside
/// method bodies. Receiver is normalized to `"this"` so the resolver in
/// `pipeline/calls.rs` (which checks `self`/`this`) matches.
#[cfg(feature = "codegraph")]
fn walk_ruby_accesses(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    accesses: &mut Vec<AccessSite>,
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
                    walk_ruby_accesses(child, file_path, source, accesses, enclosing);
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
                    walk_ruby_accesses(child, file_path, source, accesses, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "instance_variable" | "class_variable" => {
            if let Some(caller) = enclosing.last() {
                let raw = text(node, source);
                let fname = raw.trim_start_matches('@').to_string();
                if !fname.is_empty() {
                    accesses.push(AccessSite {
                        caller_qualified_name: caller.clone(),
                        field_name: fname,
                        receiver: "this".to_string(),
                        line: node.start_position().row as u32 + 1,
                        is_write: false,
                    });
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_ruby_accesses(child, file_path, source, accesses, enclosing);
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
        ..Default::default()
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
        ..Default::default()
    }
}
