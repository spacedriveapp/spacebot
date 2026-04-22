//! C# language provider.
//!
//! Extracts:
//! - `class_declaration`        → Class (with `extends` base + `implements` interfaces)
//! - `interface_declaration`    → Interface
//! - `struct_declaration`       → Struct
//! - `record_declaration`       → Record
//! - `enum_declaration`         → Enum
//! - `namespace_declaration`    → Namespace
//! - `method_declaration`       → Method
//! - `constructor_declaration`  → Method
//! - `property_declaration`     → Variable
//! - `field_declaration`        → Variable
//! - `delegate_declaration`     → Function (a delegate is a function type)
//! - `using_directive`          → Import
//!
//! Call extraction walks `invocation_expression` nodes, which carry a
//! `function` field that is either an `identifier` (bare call),
//! `member_access_expression` (receiver.method), or a generic variant.

use super::languages::SupportedLanguage;
use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider, LocalBinding};
use crate::codegraph::types::NodeLabel;

pub struct CSharpProvider;

impl LanguageProvider for CSharpProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::CSharp
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

    fn file_extensions(&self) -> &[&str] {
        &["cs"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Class,
            NodeLabel::Interface,
            NodeLabel::Struct,
            NodeLabel::Record,
            NodeLabel::Enum,
            NodeLabel::Namespace,
            NodeLabel::Method,
            NodeLabel::Variable,
            NodeLabel::Function,
            NodeLabel::Import,
        ]
    }

    fn queries(&self) -> Option<&'static super::queries::QuerySet> {
        Some(&super::queries::csharp::QUERY_SET)
    }
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_csharp_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_csharp_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    match node.kind() {
        "using_directive" => {
            // `using X.Y.Z;` or `using alias = X.Y.Z;`. Handle both by
            // looking at the raw text — splitting on `=` picks up the
            // alias form, and the trailing `.*` check is for the rare
            // wildcard-like `using static X.Y.*` variant.
            let raw = node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .trim_start_matches("using")
                .trim_start_matches("static")
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_string();
            if !raw.is_empty() {
                let (name, import_source, original) = if let Some((alias, path)) = raw.split_once('=') {
                    let alias = alias.trim().to_string();
                    let path = path.trim().to_string();
                    let orig = path.rsplit('.').next().unwrap_or(&path).to_string();
                    (alias, path, Some(orig))
                } else {
                    let leaf = raw.rsplit('.').next().unwrap_or(&raw).to_string();
                    (leaf, raw.clone(), None)
                };
                let mut metadata = std::collections::HashMap::new();
                if let Some(orig) = original
                    && orig != name
                {
                    metadata.insert("original_name".to_string(), orig);
                }
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: format!("{file_path}::import::{import_source}::{name}"),
                    label: NodeLabel::Import,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: None,
                    import_source: Some(import_source),
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata,
                    ..Default::default()
                });
            }
        }
        "namespace_declaration" | "file_scoped_namespace_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Namespace, &node, source));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_csharp_node(child, file_path, source, symbols, Some(&qn));
                    }
                } else {
                    let cursor = &mut node.walk();
                    for child in node.children(cursor) {
                        if child.id() != name_node.id() {
                            walk_csharp_node(child, file_path, source, symbols, Some(&qn));
                        }
                    }
                }
                return;
            }
        }
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                let (extends, implements) = collect_bases(node, source);
                let (vis, is_static, is_abstract, is_readonly, is_sealed, annotations) =
                    extract_csharp_modifiers(node, source.as_bytes());
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
                    is_exported: crate::codegraph::semantic::member_rules::is_exported(
                        SupportedLanguage::CSharp,
                        match vis.as_deref() {
                            Some("public") => crate::codegraph::semantic::member_rules::Visibility::Public,
                            Some("private") => crate::codegraph::semantic::member_rules::Visibility::Private,
                            Some("protected") => crate::codegraph::semantic::member_rules::Visibility::Protected,
                            Some("internal") => crate::codegraph::semantic::member_rules::Visibility::Package,
                            _ => crate::codegraph::semantic::member_rules::Visibility::Package,
                        },
                        &[],
                        &name,
                    ),
                    visibility: vis,
                    is_static,
                    is_abstract,
                    is_readonly,
                    is_final: is_sealed,
                    annotations,
                    ..Default::default()
                });
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_csharp_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "struct_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Struct, &node, source));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_csharp_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "record_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Record, &node, source));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_csharp_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Interface, &node, source));
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_csharp_node(child, file_path, source, symbols, Some(&qn));
                    }
                }
                return;
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Enum, &node, source));
            }
        }
        "delegate_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Function, &node, source));
            }
        }
        "method_declaration" | "constructor_declaration" | "destructor_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    let method_qname = qname(file_path, parent_name, &name);
                    let mut method_sym =
                        sym(file_path, parent_name, &name, NodeLabel::Method, &node, source);
                    // Only `method_declaration` carries a `returns` field;
                    // constructors and destructors have an implicit void
                    // return that offers no useful type information.
                    if node.kind() == "method_declaration"
                        && let Some(returns) = node.child_by_field_name("returns")
                    {
                        let ty = text(returns, source);
                        if !ty.is_empty() && ty != "void" {
                            method_sym
                                .metadata
                                .insert("declared_type".to_string(), ty);
                        }
                    }
                    symbols.push(method_sym);
                    if let Some(params) = node.child_by_field_name("parameters") {
                        collect_csharp_params(params, source, &method_qname, symbols);
                    }
                }
            }
        }
        "field_declaration" | "property_declaration" | "event_declaration" => {
            // Property and event declarations carry `type` directly,
            // while field_declaration nests it one level deeper inside
            // a `variable_declaration`. Read whichever applies, shared
            // across every declarator in the statement.
            let declared_type = node
                .child_by_field_name("type")
                .map(|n| text(n, source))
                .or_else(|| {
                    let cursor = &mut node.walk();
                    node.children(cursor).find_map(|c| {
                        if c.kind() == "variable_declaration" {
                            c.child_by_field_name("type").map(|t| text(t, source))
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or_default();

            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    let mut var_sym = sym(file_path, parent_name, &name, NodeLabel::Variable, &node, source);
                    if !declared_type.is_empty() {
                        var_sym
                            .metadata
                            .insert("declared_type".to_string(), declared_type.clone());
                    }
                    symbols.push(var_sym);
                }
            } else {
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    collect_field_names(
                        child,
                        file_path,
                        source,
                        symbols,
                        parent_name,
                        &declared_type,
                    );
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_csharp_node(child, file_path, source, symbols, parent_name);
    }
}

#[cfg(feature = "codegraph")]
fn collect_field_names(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
    declared_type: &str,
) {
    if node.kind() == "variable_declarator"
        && let Some(n) = node.child_by_field_name("name")
    {
        let name = text(n, source);
        if !name.is_empty() {
            let mut var_sym = sym(file_path, parent_name, &name, NodeLabel::Variable, &node, source);
            if !declared_type.is_empty() {
                var_sym
                    .metadata
                    .insert("declared_type".to_string(), declared_type.to_string());
            }
            symbols.push(var_sym);
        }
        return;
    }
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        collect_field_names(child, file_path, source, symbols, parent_name, declared_type);
    }
}

/// Collect C# method/constructor parameters as Parameter symbols parented
/// to the enclosing method. Tree-sitter-c-sharp wraps them in a
/// `parameter_list` containing `parameter` nodes (with a `name` field)
/// plus occasional `_this_parameter` nodes for extension methods (which
/// we skip — `this` is the receiver, already modeled via the class).
#[cfg(feature = "codegraph")]
fn collect_csharp_params(
    params_node: tree_sitter::Node,
    source: &str,
    method_qname: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let cursor = &mut params_node.walk();
    for child in params_node.children(cursor) {
        if child.kind() != "parameter" {
            continue;
        }
        let Some(name_node) = child.child_by_field_name("name") else {
            continue;
        };
        let pname = text(name_node, source);
        if pname.is_empty() || pname == "this" {
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
            qualified_name: format!("{method_qname}::{pname}"),
            label: NodeLabel::Parameter,
            line_start: child.start_position().row as u32 + 1,
            line_end: child.end_position().row as u32 + 1,
            parent: Some(method_qname.to_string()),
            import_source: None,
            extends: None,
            implements: Vec::new(),
            decorates: None,
            metadata,
            ..Default::default()
        });
    }
}

#[cfg(feature = "codegraph")]
fn collect_bases(class_node: tree_sitter::Node, source: &str) -> (Option<String>, Vec<String>) {
    // C# base_list contains base types as comma-separated children.
    // The first is conventionally the base class; the rest are
    // interfaces. We cannot fully disambiguate without a symbol table,
    // so we stash the first as `extends` and the rest as `implements`.
    let mut bases: Vec<String> = Vec::new();
    if let Some(base_list) = class_node.child_by_field_name("bases") {
        let cursor = &mut base_list.walk();
        for child in base_list.children(cursor) {
            let t = text(child, source).trim().trim_matches(',').trim().to_string();
            if !t.is_empty() && t != "," && t != ":" {
                bases.push(t);
            }
        }
    }
    let extends = bases.first().cloned();
    let implements = bases.into_iter().skip(1).collect();
    (extends, implements)
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(file_path: &str, content: &str) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_csharp_calls(tree.root_node(), file_path, content, &mut calls, &mut Vec::new());
    calls
}

#[cfg(feature = "codegraph")]
fn walk_csharp_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "namespace_declaration"
        | "file_scoped_namespace_declaration"
        | "class_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_csharp_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "method_declaration" | "constructor_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_csharp_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "invocation_expression" => {
            if let Some(caller) = enclosing.last()
                && let Some(func_node) = node.child_by_field_name("function")
            {
                let (name, receiver, is_method) = match func_node.kind() {
                    "identifier" => (text(func_node, source), None, false),
                    "member_access_expression" => {
                        let name = func_node
                            .child_by_field_name("name")
                            .map(|n| text(n, source))
                            .unwrap_or_default();
                        let recv = func_node
                            .child_by_field_name("expression")
                            .map(|n| text(n, source));
                        (name, recv, true)
                    }
                    _ => (text(func_node, source), None, false),
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
        "object_creation_expression" => {
            // `new Foo(...)` — the `type` field holds the class name.
            if let Some(caller) = enclosing.last()
                && let Some(type_node) = node.child_by_field_name("type")
            {
                let name = text(type_node, source);
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
        walk_csharp_calls(child, file_path, source, calls, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_accesses_tree_sitter(file_path: &str, content: &str) -> Vec<AccessSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut accesses = Vec::new();
    walk_csharp_accesses(
        tree.root_node(),
        file_path,
        content,
        &mut accesses,
        &mut Vec::new(),
    );
    accesses
}

/// Walk the C# AST collecting `this.field` accesses inside method bodies.
/// Tree-sitter-c-sharp uses `member_access_expression` with an `expression`
/// field that may be a `this_expression` keyword.
#[cfg(feature = "codegraph")]
fn walk_csharp_accesses(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    accesses: &mut Vec<AccessSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "namespace_declaration"
        | "file_scoped_namespace_declaration"
        | "class_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_csharp_accesses(child, file_path, source, accesses, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "method_declaration" | "constructor_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(qn);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_csharp_accesses(child, file_path, source, accesses, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "member_access_expression" => {
            if let Some(caller) = enclosing.last()
                && let Some(expr) = node.child_by_field_name("expression")
                && expr.kind() == "this_expression"
                && let Some(name) = node.child_by_field_name("name")
            {
                let fname = text(name, source);
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
        walk_csharp_accesses(child, file_path, source, accesses, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_locals_tree_sitter(file_path: &str, content: &str) -> Vec<LocalBinding> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut locals = Vec::new();
    walk_csharp_locals(
        tree.root_node(),
        file_path,
        content,
        &mut locals,
        &mut Vec::new(),
    );
    locals
}

#[cfg(feature = "codegraph")]
fn walk_csharp_locals(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    locals: &mut Vec<LocalBinding>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "namespace_declaration"
        | "file_scoped_namespace_declaration"
        | "class_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let outer = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(outer);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_csharp_locals(child, file_path, source, locals, enclosing);
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
                    walk_csharp_locals(child, file_path, source, locals, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "local_declaration_statement" => {
            // A local_declaration wraps one variable_declaration whose
            // `type` field applies to all declarator children. `var` is
            // skipped — resolving it needs flow analysis we don't run.
            if let Some(function_qn) = enclosing.last() {
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    if child.kind() != "variable_declaration" {
                        continue;
                    }
                    let declared_type = child
                        .child_by_field_name("type")
                        .map(|n| text(n, source))
                        .unwrap_or_default();
                    if declared_type.is_empty() || declared_type == "var" {
                        continue;
                    }
                    let decl_cursor = &mut child.walk();
                    for decl in child.children(decl_cursor) {
                        if decl.kind() != "variable_declarator" {
                            continue;
                        }
                        if let Some(name_node) = decl.child_by_field_name("name") {
                            let name = text(name_node, source);
                            if !name.is_empty() {
                                locals.push(LocalBinding {
                                    function_qualified_name: function_qn.clone(),
                                    name,
                                    declared_type: declared_type.clone(),
                                });
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
        walk_csharp_locals(child, file_path, source, locals, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_tests_tree_sitter(file_path: &str, content: &str) -> Vec<String> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut tests = Vec::new();
    walk_csharp_tests(
        tree.root_node(),
        file_path,
        content,
        &mut tests,
        &mut Vec::new(),
    );
    tests
}

#[cfg(feature = "codegraph")]
fn walk_csharp_tests(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    tests: &mut Vec<String>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "namespace_declaration"
        | "file_scoped_namespace_declaration"
        | "class_declaration"
        | "struct_declaration"
        | "record_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let outer = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(outer);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_csharp_tests(child, file_path, source, tests, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() && csharp_method_is_test(node, source) {
                    let fq = match enclosing.last() {
                        Some(p) => format!("{p}::{name}"),
                        None => format!("{file_path}::{name}"),
                    };
                    tests.push(fq);
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_csharp_tests(child, file_path, source, tests, enclosing);
    }
}

/// Detect `[Test]` (NUnit), `[Fact]`/`[Theory]` (xUnit), and
/// `[TestMethod]` (MSTest) attributes on a `method_declaration`. C#
/// attributes show up as `attribute_list` children containing
/// `attribute` nodes with a `name` field.
#[cfg(feature = "codegraph")]
fn csharp_method_is_test(method: tree_sitter::Node, source: &str) -> bool {
    let cursor = &mut method.walk();
    for child in method.children(cursor) {
        if child.kind() != "attribute_list" {
            continue;
        }
        let attr_cursor = &mut child.walk();
        for attr in child.children(attr_cursor) {
            if attr.kind() != "attribute" {
                continue;
            }
            let name = attr
                .child_by_field_name("name")
                .map(|n| text(n, source))
                .unwrap_or_default();
            let leaf = name.rsplit('.').next().unwrap_or(&name);
            if matches!(leaf, "Test" | "Fact" | "Theory" | "TestMethod" | "TestCase") {
                return true;
            }
        }
    }
    false
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
    source: &str,
) -> ExtractedSymbol {
    let (vis, is_static, is_abstract, is_readonly, is_sealed, annotations) =
        extract_csharp_modifiers(*node, source.as_bytes());
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
        is_exported: crate::codegraph::semantic::member_rules::is_exported(
            SupportedLanguage::CSharp,
            match vis.as_deref() {
                Some("public") => crate::codegraph::semantic::member_rules::Visibility::Public,
                Some("private") => crate::codegraph::semantic::member_rules::Visibility::Private,
                Some("protected") => crate::codegraph::semantic::member_rules::Visibility::Protected,
                Some("internal") => crate::codegraph::semantic::member_rules::Visibility::Package,
                _ => crate::codegraph::semantic::member_rules::Visibility::Package,
            },
            &[],
            name,
        ),
        visibility: vis,
        is_static,
        is_abstract,
        is_readonly,
        is_final: is_sealed,
        annotations,
        ..Default::default()
    }
}

/// Extract C# modifier info from a declaration node.
#[cfg(feature = "codegraph")]
fn extract_csharp_modifiers(
    node: tree_sitter::Node,
    source: &[u8],
) -> (Option<String>, bool, bool, bool, bool, Option<String>) {
    let mut vis = None;
    let mut is_static = false;
    let mut is_abstract = false;
    let mut is_readonly = false;
    let mut is_sealed = false;
    let mut anno_names: Vec<String> = Vec::new();

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        let kind = child.kind();
        if kind == "modifier" {
            let raw = child.utf8_text(source).unwrap_or("");
            match raw {
                "public" => vis = Some("public".to_string()),
                "private" => vis = Some("private".to_string()),
                "protected" => vis = Some("protected".to_string()),
                "internal" => vis = Some("internal".to_string()),
                "static" => is_static = true,
                "abstract" => is_abstract = true,
                "readonly" => is_readonly = true,
                "sealed" => is_sealed = true,
                _ => {}
            }
        } else if kind == "attribute_list" {
            let attr_cursor = &mut child.walk();
            for attr in child.children(attr_cursor) {
                if attr.kind() == "attribute"
                    && let Some(name_node) = attr.child_by_field_name("name")
                {
                    let aname = name_node.utf8_text(source).unwrap_or("").to_string();
                    if !aname.is_empty() {
                        anno_names.push(aname);
                    }
                }
            }
        }
    }
    let annotations = if anno_names.is_empty() {
        None
    } else {
        Some(anno_names.join(","))
    };
    (vis, is_static, is_abstract, is_readonly, is_sealed, annotations)
}

fn extract_fallback(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = i as u32 + 1;

        let patterns: &[(&str, NodeLabel)] = &[
            ("public class ", NodeLabel::Class),
            ("internal class ", NodeLabel::Class),
            ("class ", NodeLabel::Class),
            ("public interface ", NodeLabel::Interface),
            ("interface ", NodeLabel::Interface),
            ("public struct ", NodeLabel::Struct),
            ("struct ", NodeLabel::Struct),
            ("public enum ", NodeLabel::Enum),
            ("enum ", NodeLabel::Enum),
            ("namespace ", NodeLabel::Namespace),
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
