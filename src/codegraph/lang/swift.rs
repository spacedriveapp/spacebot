//! Swift language provider.
//!
//! Extracts:
//! - `class_declaration`    → Class
//! - `struct_declaration`   → Struct (via `class_declaration` with "struct" keyword in some grammars)
//! - `protocol_declaration` → Interface
//! - `enum_declaration`     → Enum
//! - `function_declaration` → Function / Method
//! - `import_declaration`   → Import
//! - `extension_declaration`→ Impl (Swift `extension Foo { ... }`)
//!
//! Swift's tree-sitter grammar is notoriously branchy around
//! `type_identifier` vs `user_type` vs `simple_identifier`, so the
//! provider uses a tolerant "first identifier-like child" strategy to
//! fish declaration names out.

use super::languages::SupportedLanguage;
use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider};
use crate::codegraph::types::NodeLabel;

pub struct SwiftProvider;

impl LanguageProvider for SwiftProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Swift
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
        &["swift"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Class,
            NodeLabel::Struct,
            NodeLabel::Interface,
            NodeLabel::Enum,
            NodeLabel::Function,
            NodeLabel::Method,
            NodeLabel::Impl,
            NodeLabel::Import,
        ]
    }

    fn queries(&self) -> Option<&'static super::queries::QuerySet> {
        Some(&super::queries::swift::QUERY_SET)
    }
}

#[cfg(feature = "codegraph")]
fn swift_language() -> tree_sitter::Language {
    tree_sitter_swift::LANGUAGE.into()
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language = swift_language();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_swift_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_swift_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    match node.kind() {
        "import_declaration" => {
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
                    ..Default::default()
                });
            }
        }
        "class_declaration" => {
            if let Some(name) = find_declaration_name(node, source) {
                let qn = qname(file_path, parent_name, &name);
                // Swift's grammar uses a single class_declaration kind
                // for both `class` and `struct`; detect struct via the
                // presence of the keyword in the node's leading text.
                let raw = node.utf8_text(source.as_bytes()).unwrap_or("");
                let label = if raw.trim_start().starts_with("struct") {
                    NodeLabel::Struct
                } else {
                    NodeLabel::Class
                };
                symbols.push(sym(file_path, parent_name, &name, label, &node));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_swift_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "protocol_declaration" => {
            if let Some(name) = find_declaration_name(node, source) {
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Interface, &node));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_swift_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "enum_declaration" => {
            if let Some(name) = find_declaration_name(node, source) {
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Enum, &node));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_swift_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "extension_declaration" => {
            if let Some(name) = find_declaration_name(node, source) {
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Impl, &node));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_swift_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "function_declaration" | "init_declaration" | "deinit_declaration" => {
            if let Some(name) = find_declaration_name(node, source) {
                let label = if parent_name.is_some() {
                    NodeLabel::Method
                } else {
                    NodeLabel::Function
                };
                let fn_qname = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, label, &node));
                collect_swift_params(node, source, &fn_qname, symbols);
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_swift_node(child, file_path, source, symbols, parent_name);
    }
}

/// Collect Swift function/init parameters as Parameter symbols parented
/// to the enclosing function. Tree-sitter-swift emits `parameter` nodes
/// (inside the function's parameter list) that carry a `name` field
/// holding the internal name (the one visible in the function body) and
/// optionally an `external_name` field (the call-site label).
///
/// When both are present we prefer the internal name since that's what
/// the body references. Swift's `_` wildcard external label is fine —
/// we ignore it because we never emit external_name as a symbol.
#[cfg(feature = "codegraph")]
fn collect_swift_params(
    fn_node: tree_sitter::Node,
    source: &str,
    fn_qname: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    // Swift's grammar doesn't consistently name a parameter-list node,
    // so we scan the function's direct descendants for `parameter` kinds.
    let cursor = &mut fn_node.walk();
    for child in fn_node.children(cursor) {
        collect_swift_params_in(child, source, fn_qname, symbols);
    }
}

#[cfg(feature = "codegraph")]
fn collect_swift_params_in(
    node: tree_sitter::Node,
    source: &str,
    fn_qname: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    // Stop descending at nested functions/closures so their params
    // don't leak up into the enclosing function's parameter set.
    let kind = node.kind();
    if kind == "function_declaration"
        || kind == "init_declaration"
        || kind == "deinit_declaration"
        || kind == "lambda_literal"
    {
        return;
    }

    if kind == "parameter" {
        let pname = node
            .child_by_field_name("name")
            .map(|n| text(n, source))
            .filter(|s| !s.is_empty())
            .or_else(|| {
                // Some grammar revisions expose the name only as a direct
                // simple_identifier child. Fall back to the first one.
                let cur = &mut node.walk();
                for c in node.children(cur) {
                    if c.kind() == "simple_identifier" || c.kind() == "identifier" {
                        let t = text(c, source);
                        if !t.is_empty() {
                            return Some(t);
                        }
                    }
                }
                None
            });
        if let Some(pname) = pname
            && pname != "_"
        {
            symbols.push(ExtractedSymbol {
                name: pname.clone(),
                qualified_name: format!("{fn_qname}::{pname}"),
                label: NodeLabel::Parameter,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                parent: Some(fn_qname.to_string()),
                import_source: None,
                extends: None,
                implements: Vec::new(),
                decorates: None,
                metadata: std::collections::HashMap::new(),
                ..Default::default()
            });
        }
        return;
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        collect_swift_params_in(child, source, fn_qname, symbols);
    }
}

#[cfg(feature = "codegraph")]
fn find_declaration_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    if let Some(n) = node.child_by_field_name("name") {
        let t = text(n, source);
        if !t.is_empty() {
            return Some(t);
        }
    }
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        let kind = child.kind();
        if matches!(
            kind,
            "simple_identifier" | "type_identifier" | "identifier" | "user_type"
        ) {
            let t = text(child, source).trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(file_path: &str, content: &str) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language = swift_language();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_swift_calls(tree.root_node(), file_path, content, &mut calls, &mut Vec::new());
    calls
}

#[cfg(feature = "codegraph")]
fn walk_swift_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class_declaration"
        | "protocol_declaration"
        | "enum_declaration"
        | "extension_declaration" => {
            if let Some(name) = find_declaration_name(node, source) {
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_swift_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "function_declaration" | "init_declaration" => {
            if let Some(name) = find_declaration_name(node, source) {
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_swift_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "call_expression" => {
            if let Some(caller) = enclosing.last()
                && let Some(callee_expr) = node.child(0)
            {
                let (name, receiver, is_method) = match callee_expr.kind() {
                    "simple_identifier" | "identifier" => {
                        (text(callee_expr, source), None, false)
                    }
                    "navigation_expression" => {
                        let recv = callee_expr.child(0).map(|n| text(n, source));
                        let name = last_identifier_text(callee_expr, source);
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
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_swift_calls(child, file_path, source, calls, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_accesses_tree_sitter(file_path: &str, content: &str) -> Vec<AccessSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_swift::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut accesses = Vec::new();
    walk_swift_accesses(
        tree.root_node(),
        file_path,
        content,
        &mut accesses,
        &mut Vec::new(),
    );
    accesses
}

/// Walk the Swift AST collecting `self.field` accesses inside method
/// bodies. Tree-sitter-swift uses `navigation_expression` for `recv.x`,
/// where the receiver is the first child. We match when the receiver is
/// the `self` keyword.
#[cfg(feature = "codegraph")]
fn walk_swift_accesses(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    accesses: &mut Vec<AccessSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class_declaration"
        | "protocol_declaration"
        | "enum_declaration"
        | "extension_declaration" => {
            if let Some(name) = find_declaration_name(node, source) {
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_swift_accesses(child, file_path, source, accesses, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "function_declaration" | "init_declaration" => {
            if let Some(name) = find_declaration_name(node, source) {
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_swift_accesses(child, file_path, source, accesses, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "navigation_expression" => {
            if let Some(caller) = enclosing.last()
                && let Some(receiver_node) = node.child(0)
            {
                let recv_kind = receiver_node.kind();
                let is_self = recv_kind == "self_expression"
                    || (recv_kind == "simple_identifier"
                        && text(receiver_node, source) == "self");
                if is_self {
                    let fname = last_identifier_text(node, source);
                    if !fname.is_empty() && fname != "self" {
                        accesses.push(AccessSite {
                            caller_qualified_name: caller.clone(),
                            field_name: fname,
                            receiver: "self".to_string(),
                            line: node.start_position().row as u32 + 1,
                            is_write: false,
                        });
                    }
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_swift_accesses(child, file_path, source, accesses, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn last_identifier_text(node: tree_sitter::Node, source: &str) -> String {
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
            ("struct ", NodeLabel::Struct),
            ("protocol ", NodeLabel::Interface),
            ("enum ", NodeLabel::Enum),
            ("extension ", NodeLabel::Impl),
            ("func ", NodeLabel::Function),
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
