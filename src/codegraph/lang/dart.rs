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
use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider};
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

    fn queries(&self) -> Option<&'static super::queries::QuerySet> {
        Some(&super::queries::dart::QUERY_SET)
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
                        ..Default::default()
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
                    ..Default::default()
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
                let fn_qname = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, label, &node));
                collect_dart_params(node, source, &fn_qname, symbols);
            }
        }
        "method_signature" => {
            if let Some(name) = find_ident(node, source, &["name", "identifier"]) {
                let method_qname = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Method, &node));
                collect_dart_params(node, source, &method_qname, symbols);
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

/// Collect Dart function/method parameters as Parameter symbols parented
/// to the enclosing callable. Tree-sitter-dart nests params inside a
/// `formal_parameter_list` with children of several related kinds
/// (`formal_parameter`, `normal_formal_parameter`, `optional_formal_parameters`,
/// `named_formal_parameters`, `required_formal_parameter`, and the
/// constructor-field-init variants `this_formal_parameter` /
/// `this_final_formal_parameter`). Rather than enumerate every grammar
/// variant, we locate `formal_parameter_list` nodes inside the signature
/// and emit one Parameter per direct parameter-like child, reading the
/// first identifier descendant as the name.
#[cfg(feature = "codegraph")]
fn collect_dart_params(
    signature_node: tree_sitter::Node,
    source: &str,
    fn_qname: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let Some(list) = find_descendant_by_kind(signature_node, "formal_parameter_list") else {
        return;
    };
    let cursor = &mut list.walk();
    for child in list.children(cursor) {
        let kind = child.kind();
        // Skip literal punctuation children inside the list.
        if kind == "(" || kind == ")" || kind == "," || kind == "[" || kind == "]"
            || kind == "{" || kind == "}"
        {
            continue;
        }
        let pname = find_first_dart_identifier(child, source);
        if pname.is_empty() {
            continue;
        }
        symbols.push(ExtractedSymbol {
            name: pname.clone(),
            qualified_name: format!("{fn_qname}::{pname}"),
            label: NodeLabel::Parameter,
            line_start: child.start_position().row as u32 + 1,
            line_end: child.end_position().row as u32 + 1,
            parent: Some(fn_qname.to_string()),
            import_source: None,
            extends: None,
            implements: Vec::new(),
            decorates: None,
            metadata: std::collections::HashMap::new(),
            ..Default::default()
        });
    }
}

/// Walk descendants looking for the first node of the given kind.
/// Used to find `formal_parameter_list` inside a signature regardless
/// of the grammar nesting.
#[cfg(feature = "codegraph")]
fn find_descendant_by_kind<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    if node.kind() == kind {
        return Some(node);
    }
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        if let Some(found) = find_descendant_by_kind(child, kind) {
            return Some(found);
        }
    }
    None
}

/// Recursively find the first `identifier` node inside a Dart parameter
/// node. Handles the various grammar shapes (typed / default / named /
/// this.X constructor-field-init) without enumerating each.
#[cfg(feature = "codegraph")]
fn find_first_dart_identifier(node: tree_sitter::Node, source: &str) -> String {
    if node.kind() == "identifier" {
        return text(node, source);
    }
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        let r = find_first_dart_identifier(child, source);
        if !r.is_empty() {
            return r;
        }
    }
    String::new()
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
fn extract_accesses_tree_sitter(file_path: &str, content: &str) -> Vec<AccessSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_dart::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut accesses = Vec::new();
    walk_dart_accesses(
        tree.root_node(),
        file_path,
        content,
        &mut accesses,
        &mut Vec::new(),
    );
    accesses
}

/// Walk the Dart AST collecting `this.field` accesses inside method
/// bodies. Tree-sitter-dart's grammar varies between versions, so this
/// uses a structural-pattern-match approach: any node whose first child
/// is the `this` keyword followed by `.<identifier>` is treated as a
/// field access. This is intentionally permissive to survive grammar
/// updates.
#[cfg(feature = "codegraph")]
fn walk_dart_accesses(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    accesses: &mut Vec<AccessSite>,
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
                    walk_dart_accesses(child, file_path, source, accesses, enclosing);
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
                    walk_dart_accesses(child, file_path, source, accesses, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        _ => {}
    }

    // Permissive pattern match: a node with first child = `this`,
    // followed by `.`, followed by an `identifier`. Catches
    // assignable_expression and similar shapes across grammar versions.
    if let Some(caller) = enclosing.last()
        && node.child_count() >= 3
        && let Some(c0) = node.child(0)
        && c0.kind() == "this"
        && let Some(c1) = node.child(1)
        && text(c1, source) == "."
        && let Some(c2) = node.child(2)
        && c2.kind() == "identifier"
    {
        let fname = text(c2, source);
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

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_dart_accesses(child, file_path, source, accesses, enclosing);
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
        ..Default::default()
    }
}
