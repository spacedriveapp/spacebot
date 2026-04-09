//! Rust language provider.

use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider, LocalBinding};
use super::languages::SupportedLanguage;
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

    fn extract_locals(&self, file_path: &str, content: &str) -> Vec<LocalBinding> {
        #[cfg(feature = "codegraph")]
        {
            extract_locals_tree_sitter(file_path, content)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            let _ = (file_path, content);
            Vec::new()
        }
    }

    fn extract_tests(&self, file_path: &str, content: &str) -> Vec<String> {
        #[cfg(feature = "codegraph")]
        {
            extract_tests_tree_sitter(file_path, content)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            let _ = (file_path, content);
            Vec::new()
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
            NodeLabel::Variable,
            NodeLabel::Module,
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
                let struct_qname = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Struct, &node));
                // Extract struct fields.
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        if child.kind() == "field_declaration"
                            && let Some(fn_node) = child.child_by_field_name("name")
                        {
                            let fname = text(fn_node, source);
                            let mut field_sym = sym(file_path, Some(&struct_qname), &fname, NodeLabel::Variable, &child);
                            if let Some(type_node) = child.child_by_field_name("type") {
                                let ty = text(type_node, source);
                                if !ty.is_empty() {
                                    field_sym.metadata.insert("declared_type".to_string(), ty);
                                }
                            }
                            symbols.push(field_sym);
                        }
                    }
                }
            }
        }
        "enum_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let enum_qname = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Enum, &node));
                // Extract enum variants.
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        if child.kind() == "enum_variant"
                            && let Some(vn) = child.child_by_field_name("name")
                        {
                            let vname = text(vn, source);
                            symbols.push(sym(file_path, Some(&enum_qname), &vname, NodeLabel::Variable, &child));
                        }
                    }
                }
            }
        }
        "mod_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let mod_qname = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Module, &node));
                // Recurse into module body.
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_rust_node(child, file_path, source, symbols, Some(&mod_qname));
                    }
                }
                return;
            }
        }
        "let_declaration" => {
            if let Some(name_node) = node.child_by_field_name("pattern") {
                let name = text(name_node, source);
                // Only extract named bindings, skip destructuring
                if !name.is_empty() && !name.contains('(') && !name.contains('{') {
                    symbols.push(sym(file_path, parent_name, &name, NodeLabel::Variable, &node));
                }
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
                let mut fn_sym = sym(file_path, parent_name, &name, label, &node);
                if let Some(return_type) = node.child_by_field_name("return_type") {
                    let ty = text(return_type, source);
                    if !ty.is_empty() {
                        fn_sym.metadata.insert("declared_type".to_string(), ty);
                    }
                }
                symbols.push(fn_sym);

                let fn_qname = qname(file_path, parent_name, &name);
                if let Some(params) = node.child_by_field_name("parameters") {
                    collect_rust_params(params, source, &fn_qname, symbols);
                }
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
            if let Some(argument) = node.child_by_field_name("argument") {
                let line_start = node.start_position().row as u32 + 1;
                let line_end = node.end_position().row as u32 + 1;
                collect_rust_uses(argument, "", file_path, line_start, line_end, source, symbols);
            }
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

/// Walk a `use_declaration`'s argument subtree and emit one Import node
/// per leaf binding. `prefix` is the `::`-joined path accumulated from
/// outer `scoped_use_list`s so nested groupings collapse to full paths.
#[cfg(feature = "codegraph")]
fn collect_rust_uses(
    node: tree_sitter::Node,
    prefix: &str,
    file_path: &str,
    line_start: u32,
    line_end: u32,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let push = |name: &str, import_source: &str, original: Option<&str>, symbols: &mut Vec<ExtractedSymbol>| {
        if name.is_empty() || import_source.is_empty() {
            return;
        }
        let mut metadata = std::collections::HashMap::new();
        if let Some(orig) = original {
            metadata.insert("original_name".to_string(), orig.to_string());
        }
        symbols.push(ExtractedSymbol {
            name: name.to_string(),
            qualified_name: format!("{file_path}::use::{import_source}::{name}"),
            label: NodeLabel::Import,
            line_start,
            line_end,
            parent: None,
            import_source: Some(import_source.to_string()),
            extends: None,
            implements: Vec::new(),
            decorates: None,
            metadata,
        });
    };

    let join = |a: &str, b: &str| -> String {
        if a.is_empty() {
            b.to_string()
        } else {
            format!("{a}::{b}")
        }
    };

    match node.kind() {
        "identifier" | "self" | "super" | "crate" => {
            let name = text(node, source);
            let full = join(prefix, &name);
            push(&name, &full, None, symbols);
        }
        "scoped_identifier" => {
            let path_text = text(node, source);
            let full = join(prefix, &path_text);
            let leaf = node
                .child_by_field_name("name")
                .map(|n| text(n, source))
                .unwrap_or_default();
            push(&leaf, &full, None, symbols);
        }
        "use_as_clause" => {
            let path_node = node.child_by_field_name("path");
            let path_text = path_node.map(|n| text(n, source)).unwrap_or_default();
            let full = join(prefix, &path_text);
            let alias = node
                .child_by_field_name("alias")
                .map(|n| text(n, source))
                .unwrap_or_default();
            let orig = full.rsplit("::").next().unwrap_or("").to_string();
            let original = if alias != orig { Some(orig.as_str()) } else { None };
            push(&alias, &full, original, symbols);
        }
        "scoped_use_list" => {
            let path_text = node
                .child_by_field_name("path")
                .map(|n| text(n, source))
                .unwrap_or_default();
            let new_prefix = join(prefix, &path_text);
            if let Some(list) = node.child_by_field_name("list") {
                let cursor = &mut list.walk();
                for child in list.children(cursor) {
                    collect_rust_uses(child, &new_prefix, file_path, line_start, line_end, source, symbols);
                }
            }
        }
        "use_list" => {
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                collect_rust_uses(child, prefix, file_path, line_start, line_end, source, symbols);
            }
        }
        "use_wildcard" => {
            // `path::*` — find the path child (identifier or scoped_identifier)
            // and emit one wildcard node.
            let cursor = &mut node.walk();
            let path_text = node
                .children(cursor)
                .find(|c| c.kind() == "identifier" || c.kind() == "scoped_identifier")
                .map(|c| text(c, source))
                .unwrap_or_default();
            let full = join(prefix, &path_text);
            push("*", &full, None, symbols);
        }
        _ => {}
    }
}

/// Collect Rust function parameters as Parameter symbols.
///
/// Tree-sitter-rust wraps parameters in `parameters` containing:
/// - `parameter` for `x: Type`
/// - `self_parameter` for `self`, `&self`, `&mut self` (skipped — receiver)
///
/// Each parameter has a `pattern` field that's typically an identifier.
#[cfg(feature = "codegraph")]
fn collect_rust_params(
    params_node: tree_sitter::Node,
    source: &str,
    function_qname: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let cursor = &mut params_node.walk();
    for child in params_node.children(cursor) {
        if child.kind() != "parameter" {
            continue; // skip self_parameter, commas, parens
        }

        // Pattern could be an identifier or destructuring pattern.
        // For now we only handle the identifier case.
        let pname = child
            .child_by_field_name("pattern")
            .filter(|n| n.kind() == "identifier")
            .map(|n| text(n, source));

        let Some(pname) = pname else { continue };
        if pname.is_empty() || pname == "_" {
            continue;
        }

        let mut metadata = std::collections::HashMap::new();
        if let Some(type_node) = child.child_by_field_name("type") {
            let ty = text(type_node, source);
            if !ty.is_empty() {
                metadata.insert("declared_type".to_string(), ty);
            }
        }

        symbols.push(ExtractedSymbol {
            name: pname.clone(),
            qualified_name: format!("{function_qname}::{pname}"),
            label: NodeLabel::Parameter,
            line_start: child.start_position().row as u32 + 1,
            line_end: child.end_position().row as u32 + 1,
            parent: Some(function_qname.to_string()),
            import_source: None,
            extends: None,
            implements: Vec::new(),
            decorates: None,
            metadata,
        });
    }
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(file_path: &str, content: &str) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_rust_calls(tree.root_node(), file_path, content, &mut calls, &mut Vec::new());
    calls
}

#[cfg(feature = "codegraph")]
fn walk_rust_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    let kind = node.kind();

    match kind {
        "mod_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let mq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(mq);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_rust_calls(child, file_path, source, calls, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "impl_item" => {
            let impl_name = node
                .child_by_field_name("type")
                .map(|n| text(n, source))
                .unwrap_or_else(|| "impl".to_string());
            let iq = match enclosing.last() {
                Some(p) => format!("{p}::{impl_name}"),
                None => format!("{file_path}::{impl_name}"),
            };
            enclosing.push(iq);
            if let Some(body) = node.child_by_field_name("body") {
                let cursor = &mut body.walk();
                for child in body.children(cursor) {
                    walk_rust_calls(child, file_path, source, calls, enclosing);
                }
            }
            enclosing.pop();
            return;
        }
        "trait_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let tq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(tq);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_rust_calls(child, file_path, source, calls, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "function_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                // Walk entire function (body is inside)
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_rust_calls(child, file_path, source, calls, enclosing);
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
                    "scoped_identifier" => {
                        // Path call like `Module::func()` or `Type::new()`
                        let name = func_node
                            .child_by_field_name("name")
                            .map(|n| text(n, source))
                            .unwrap_or_default();
                        let path = func_node
                            .child_by_field_name("path")
                            .map(|n| text(n, source));
                        if !name.is_empty() {
                            calls.push(CallSite {
                                caller_qualified_name: caller.clone(),
                                callee_name: name,
                                line: node.start_position().row as u32 + 1,
                                is_method_call: path.is_some(),
                                receiver: path,
                            });
                        }
                    }
                    "field_expression" => {
                        // Method-style call like `obj.method()`
                        let method = func_node
                            .child_by_field_name("field")
                            .map(|n| text(n, source))
                            .unwrap_or_default();
                        let recv = func_node
                            .child_by_field_name("value")
                            .map(|n| text(n, source));
                        if !method.is_empty() {
                            calls.push(CallSite {
                                caller_qualified_name: caller.clone(),
                                callee_name: method,
                                line: node.start_position().row as u32 + 1,
                                is_method_call: true,
                                receiver: recv,
                            });
                        }
                    }
                    _ => {}
                }
            }
            // Fall through to recurse into nested calls
        }
        "macro_invocation" => {
            // Capture macro calls like `println!(...)`, `vec![...]`
            if let Some(caller) = enclosing.last()
                && let Some(macro_node) = node.child_by_field_name("macro")
            {
                let name = text(macro_node, source);
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
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_rust_calls(child, file_path, source, calls, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_accesses_tree_sitter(file_path: &str, content: &str) -> Vec<AccessSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut accesses = Vec::new();
    walk_rust_accesses(
        tree.root_node(),
        file_path,
        content,
        &mut accesses,
        &mut Vec::new(),
    );
    accesses
}

/// Walk the Rust AST collecting `self.field` accesses inside method bodies.
/// Mirrors `walk_rust_calls` but emits `AccessSite` records instead of CallSites.
///
/// Note: a method call `self.foo()` is parsed as a `call_expression` whose
/// `function` is a `field_expression` `self.foo`. We still emit an
/// AccessSite for `foo`, but the access resolver only emits an edge if
/// there's actually a Variable named `foo` on the parent class — methods
/// won't be in `variables_by_class`, so they get filtered out at resolution
/// time. This keeps the walker simple at the cost of a few wasted lookups.
#[cfg(feature = "codegraph")]
fn walk_rust_accesses(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    accesses: &mut Vec<AccessSite>,
    enclosing: &mut Vec<String>,
) {
    let kind = node.kind();

    match kind {
        "mod_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let mq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(mq);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_rust_accesses(child, file_path, source, accesses, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "impl_item" => {
            let impl_name = node
                .child_by_field_name("type")
                .map(|n| text(n, source))
                .unwrap_or_else(|| "impl".to_string());
            let iq = match enclosing.last() {
                Some(p) => format!("{p}::{impl_name}"),
                None => format!("{file_path}::{impl_name}"),
            };
            enclosing.push(iq);
            if let Some(body) = node.child_by_field_name("body") {
                let cursor = &mut body.walk();
                for child in body.children(cursor) {
                    walk_rust_accesses(child, file_path, source, accesses, enclosing);
                }
            }
            enclosing.pop();
            return;
        }
        "trait_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let tq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(tq);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_rust_accesses(child, file_path, source, accesses, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "function_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_rust_accesses(child, file_path, source, accesses, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "field_expression" => {
            // `self.field` — emit access only when value is bare `self`.
            if let Some(caller) = enclosing.last()
                && let Some(value) = node.child_by_field_name("value")
                && value.kind() == "self"
                && let Some(field) = node.child_by_field_name("field")
            {
                let fname = text(field, source);
                if !fname.is_empty() {
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
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_rust_accesses(child, file_path, source, accesses, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_locals_tree_sitter(file_path: &str, content: &str) -> Vec<LocalBinding> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut locals = Vec::new();
    walk_rust_locals(
        tree.root_node(),
        file_path,
        content,
        &mut locals,
        &mut Vec::new(),
    );
    locals
}

#[cfg(feature = "codegraph")]
fn walk_rust_locals(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    locals: &mut Vec<LocalBinding>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "mod_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let mq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(mq);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_rust_locals(child, file_path, source, locals, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "impl_item" => {
            let impl_name = node
                .child_by_field_name("type")
                .map(|n| text(n, source))
                .unwrap_or_else(|| "impl".to_string());
            let iq = match enclosing.last() {
                Some(p) => format!("{p}::{impl_name}"),
                None => format!("{file_path}::{impl_name}"),
            };
            enclosing.push(iq);
            if let Some(body) = node.child_by_field_name("body") {
                let cursor = &mut body.walk();
                for child in body.children(cursor) {
                    walk_rust_locals(child, file_path, source, locals, enclosing);
                }
            }
            enclosing.pop();
            return;
        }
        "trait_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let tq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(tq);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_rust_locals(child, file_path, source, locals, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "function_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_rust_locals(child, file_path, source, locals, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "let_declaration" => {
            // Only emit when the let has an explicit `: Type` annotation
            // — untyped bindings would require flow analysis to resolve.
            if let Some(function_qn) = enclosing.last()
                && let Some(type_node) = node.child_by_field_name("type")
                && let Some(pattern) = node.child_by_field_name("pattern")
                && pattern.kind() == "identifier"
            {
                let name = text(pattern, source);
                let declared_type = text(type_node, source);
                if !name.is_empty() && !declared_type.is_empty() && name != "_" {
                    locals.push(LocalBinding {
                        function_qualified_name: function_qn.clone(),
                        name,
                        declared_type,
                    });
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_rust_locals(child, file_path, source, locals, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_tests_tree_sitter(file_path: &str, content: &str) -> Vec<String> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut tests = Vec::new();
    walk_rust_tests(
        tree.root_node(),
        file_path,
        content,
        &mut tests,
        &mut Vec::new(),
    );
    tests
}

#[cfg(feature = "codegraph")]
fn walk_rust_tests(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    tests: &mut Vec<String>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "mod_item" | "impl_item" | "trait_item" => {
            let name = match node.kind() {
                "impl_item" => node
                    .child_by_field_name("type")
                    .map(|n| text(n, source))
                    .unwrap_or_else(|| "impl".to_string()),
                _ => node
                    .child_by_field_name("name")
                    .map(|n| text(n, source))
                    .unwrap_or_default(),
            };
            if !name.is_empty() {
                let outer = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(outer);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_rust_tests(child, file_path, source, tests, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "function_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                // Recurse into the body first so nested functions still
                // get visited even when this one isn't a test itself.
                enclosing.push(fq.clone());
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_rust_tests(child, file_path, source, tests, enclosing);
                }
                enclosing.pop();
                if rust_has_test_attribute(node, source) {
                    tests.push(fq);
                }
                return;
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_rust_tests(child, file_path, source, tests, enclosing);
    }
}

/// Return true if the given `function_item` has a preceding `#[test]`
/// or `#[tokio::test]` (or any attribute whose path leaf is `test`)
/// attribute_item sibling. Tree-sitter-rust attaches attributes as
/// sibling nodes in the parent declaration list, not as children of
/// the function itself, so we walk backwards from the function.
#[cfg(feature = "codegraph")]
fn rust_has_test_attribute(function_item: tree_sitter::Node, source: &str) -> bool {
    let mut cursor = function_item;
    while let Some(prev) = cursor.prev_named_sibling() {
        match prev.kind() {
            "attribute_item" | "inner_attribute_item" => {
                let text_src = text(prev, source);
                // The attribute text is `#[test]`, `#[tokio::test]`,
                // `#[test(foo = "bar")]`, etc. A leaf-name match is
                // enough — no need to parse the meta_item tree.
                let inner = text_src
                    .trim_start_matches("#!")
                    .trim_start_matches('#')
                    .trim_start_matches('[')
                    .trim_end_matches(']');
                let path = inner.split(['(', '=']).next().unwrap_or(inner).trim();
                let leaf = path.rsplit("::").next().unwrap_or(path).trim();
                if leaf == "test" {
                    return true;
                }
                cursor = prev;
            }
            _ => return false,
        }
    }
    false
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
