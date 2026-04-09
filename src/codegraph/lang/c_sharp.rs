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
use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider};
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
            // using X.Y.Z;
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
                });
            }
        }
        "namespace_declaration" | "file_scoped_namespace_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qn = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Namespace, &node));
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
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Struct, &node));
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
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Record, &node));
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
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Interface, &node));
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
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Enum, &node));
            }
        }
        "delegate_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Function, &node));
            }
        }
        "method_declaration" | "constructor_declaration" | "destructor_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    let method_qname = qname(file_path, parent_name, &name);
                    symbols.push(sym(file_path, parent_name, &name, NodeLabel::Method, &node));
                    if let Some(params) = node.child_by_field_name("parameters") {
                        collect_csharp_params(params, source, &method_qname, symbols);
                    }
                }
            }
        }
        "field_declaration" | "property_declaration" | "event_declaration" => {
            // Resolve the declared type for call-site resolution.
            // `property_declaration` / `event_declaration` carry a
            // `type` field directly. `field_declaration` wraps a
            // `variable_declaration` whose `type` field holds the type
            // shared by all declarators.
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

            // Walk descendants for variable_declarator(s) / identifier name.
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    let mut var_sym = sym(file_path, parent_name, &name, NodeLabel::Variable, &node);
                    if !declared_type.is_empty() {
                        var_sym
                            .metadata
                            .insert("declared_type".to_string(), declared_type.clone());
                    }
                    symbols.push(var_sym);
                }
            } else {
                // Field declarations have a declaration child whose
                // variable_declarator children carry identifiers.
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
            let mut var_sym = sym(file_path, parent_name, &name, NodeLabel::Variable, &node);
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
        // C# parameters carry a `type` field (e.g. `Foo`, `List<Bar>`,
        // `Foo?`). Stash it for the call-site resolver.
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
    }
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
    }
}
