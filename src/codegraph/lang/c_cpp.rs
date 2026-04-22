//! C and C++ language providers.
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

use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider, LocalBinding};
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

    fn extract_accesses(&self, file_path: &str, content: &str) -> Vec<AccessSite> {
        #[cfg(feature = "codegraph")]
        {
            extract_accesses_tree_sitter(file_path, content, Dialect::C)
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
            extract_locals_tree_sitter(file_path, content, Dialect::C)
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

    fn queries(&self) -> Option<&'static super::queries::QuerySet> {
        Some(&super::queries::c::QUERY_SET)
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

    fn extract_accesses(&self, file_path: &str, content: &str) -> Vec<AccessSite> {
        #[cfg(feature = "codegraph")]
        {
            extract_accesses_tree_sitter(file_path, content, Dialect::Cpp)
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
            extract_locals_tree_sitter(file_path, content, Dialect::Cpp)
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
            NodeLabel::Template,
            NodeLabel::Variable,
            NodeLabel::Import,
        ]
    }

    fn queries(&self) -> Option<&'static super::queries::QuerySet> {
        Some(&super::queries::cpp::QUERY_SET)
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
                        ..Default::default()
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
                let fn_qname = qname(file_path, parent_name, &name);
                let mut fn_sym = sym(file_path, parent_name, &name, label, &node);
                if let Some(type_node) = node.child_by_field_name("type") {
                    let ty = text(type_node, source);
                    if !ty.is_empty() && ty != "void" {
                        fn_sym.metadata.insert("declared_type".to_string(), ty);
                    }
                }
                symbols.push(fn_sym);
                if let Some(fn_declarator) = find_function_declarator(declarator)
                    && let Some(params) = fn_declarator.child_by_field_name("parameters")
                {
                    collect_c_params(params, source, &fn_qname, symbols);
                }
            }
        }
        "declaration" => {
            // Function prototypes look like `declaration → function_declarator`.
            if let Some(declarator) = node.child_by_field_name("declarator")
                && declarator.kind() == "function_declarator"
                && let Some(name) = extract_function_name(declarator, source)
            {
                let fn_qname = qname(file_path, parent_name, &name);
                let mut fn_sym = sym(file_path, parent_name, &name, NodeLabel::Function, &node);
                if let Some(type_node) = node.child_by_field_name("type") {
                    let ty = text(type_node, source);
                    if !ty.is_empty() && ty != "void" {
                        fn_sym.metadata.insert("declared_type".to_string(), ty);
                    }
                }
                symbols.push(fn_sym);
                if let Some(params) = declarator.child_by_field_name("parameters") {
                    collect_c_params(params, source, &fn_qname, symbols);
                }
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
        "template_declaration" => {
            // C++ template — extract a Template node wrapping the inner
            // class/function/struct declaration. The inner declaration is
            // still walked normally via the default recursion so its
            // symbols land in the graph as children of the template.
            let inner = (0..node.child_count())
                .filter_map(|i| node.child(i))
                .find(|c| matches!(
                    c.kind(),
                    "class_specifier" | "struct_specifier" | "function_definition"
                        | "declaration" | "alias_declaration"
                ));
            if let Some(inner_node) = inner {
                let name = inner_node
                    .child_by_field_name("name")
                    .map(|n| text(n, source))
                    .or_else(|| {
                        inner_node
                            .child_by_field_name("declarator")
                            .and_then(|d| extract_function_name(d, source))
                    });
                if let Some(name) = name
                    && !name.is_empty()
                {
                    symbols.push(sym(
                        file_path, parent_name, &name, NodeLabel::Template, &node,
                    ));
                }
            }
        }
        "field_declaration" => {
            if let Some(declarator) = node.child_by_field_name("declarator")
                && let Some(name) = extract_plain_name(declarator, source)
            {
                let mut var_sym = sym(file_path, parent_name, &name, NodeLabel::Variable, &node);
                if let Some(type_node) = node.child_by_field_name("type") {
                    let ty = text(type_node, source);
                    if !ty.is_empty() {
                        var_sym.metadata.insert("declared_type".to_string(), ty);
                    }
                }
                symbols.push(var_sym);
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_c_node(child, file_path, source, symbols, parent_name);
    }
}

/// Walk down through pointer/reference layers to find the underlying
/// `function_declarator`. Returns `None` if the declarator isn't (and
/// doesn't wrap) a function.
#[cfg(feature = "codegraph")]
fn find_function_declarator<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
    match node.kind() {
        "function_declarator" => Some(node),
        "pointer_declarator" | "reference_declarator" => node
            .child_by_field_name("declarator")
            .and_then(find_function_declarator),
        _ => None,
    }
}

/// Collect C/C++ function parameters as Parameter symbols parented to
/// the enclosing function. `params_node` must be a `parameter_list`.
/// Each `parameter_declaration` carries a `declarator` field whose
/// identifier is the parameter name (possibly wrapped in pointer /
/// reference / array layers, which `extract_plain_name` unwinds).
///
/// We skip `parameter_declaration` nodes that are really just a type
/// (`void foo(int)` — no name given), since there's nothing to link.
#[cfg(feature = "codegraph")]
fn collect_c_params(
    params_node: tree_sitter::Node,
    source: &str,
    fn_qname: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let cursor = &mut params_node.walk();
    for child in params_node.children(cursor) {
        if child.kind() != "parameter_declaration" {
            continue;
        }
        let Some(declarator) = child.child_by_field_name("declarator") else {
            continue;
        };
        let Some(pname) = extract_plain_name(declarator, source) else {
            continue;
        };
        if pname.is_empty() {
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
            qualified_name: format!("{fn_qname}::{pname}"),
            label: NodeLabel::Parameter,
            line_start: child.start_position().row as u32 + 1,
            line_end: child.end_position().row as u32 + 1,
            parent: Some(fn_qname.to_string()),
            import_source: None,
            extends: None,
            implements: Vec::new(),
            decorates: None,
            metadata,
            ..Default::default()
        });
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
        "new_expression" => {
            // `new Foo(...)` — the `type` field holds the class/struct.
            if let Some(caller) = enclosing.last()
                && let Some(type_node) = node.child_by_field_name("type")
            {
                let name = text(type_node, source);
                let leaf = name.rsplit("::").next().unwrap_or(&name).to_string();
                if !leaf.is_empty() {
                    calls.push(CallSite {
                        caller_qualified_name: caller.clone(),
                        callee_name: leaf,
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
        walk_c_calls(child, file_path, source, calls, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_accesses_tree_sitter(
    file_path: &str,
    content: &str,
    dialect: Dialect,
) -> Vec<AccessSite> {
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

    let mut accesses = Vec::new();
    walk_c_accesses(
        tree.root_node(),
        file_path,
        content,
        &mut accesses,
        &mut Vec::new(),
    );
    accesses
}

/// Walk the C/C++ AST collecting `this->field` accesses inside method
/// bodies. Tree-sitter-cpp uses `field_expression` for both `obj.x` and
/// `obj->x`; we only emit when the argument is the `this` keyword (C++)
/// since C has no `this`. C input still routes through this walker but
/// will produce zero accesses.
#[cfg(feature = "codegraph")]
fn walk_c_accesses(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    accesses: &mut Vec<AccessSite>,
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
                    walk_c_accesses(child, file_path, source, accesses, enclosing);
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
                        walk_c_accesses(child, file_path, source, accesses, enclosing);
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
                    walk_c_accesses(child, file_path, source, accesses, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "field_expression" => {
            if let Some(caller) = enclosing.last()
                && let Some(arg) = node.child_by_field_name("argument")
                && arg.kind() == "this"
                && let Some(field) = node.child_by_field_name("field")
            {
                let fname = text(field, source);
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
        walk_c_accesses(child, file_path, source, accesses, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_locals_tree_sitter(
    file_path: &str,
    content: &str,
    dialect: Dialect,
) -> Vec<LocalBinding> {
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

    let mut locals = Vec::new();
    walk_c_locals(
        tree.root_node(),
        file_path,
        content,
        &mut locals,
        &mut Vec::new(),
        false,
    );
    locals
}

#[cfg(feature = "codegraph")]
fn walk_c_locals(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    locals: &mut Vec<LocalBinding>,
    enclosing: &mut Vec<String>,
    in_function_body: bool,
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
                    walk_c_locals(child, file_path, source, locals, enclosing, false);
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
                        walk_c_locals(child, file_path, source, locals, enclosing, false);
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
                    walk_c_locals(child, file_path, source, locals, enclosing, true);
                }
                enclosing.pop();
                return;
            }
        }
        "declaration" if in_function_body => {
            // Inside a function body, a `declaration` is a local.
            // Outside one it would be a prototype or file-scope global,
            // so the flag is required to discriminate the two.
            if let Some(function_qn) = enclosing.last() {
                let declared_type = node
                    .child_by_field_name("type")
                    .map(|n| text(n, source))
                    .unwrap_or_default();
                if !declared_type.is_empty()
                    && let Some(decl_node) = node.child_by_field_name("declarator")
                    && let Some(name) = extract_plain_name(decl_node, source)
                    && !name.is_empty()
                {
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
        walk_c_locals(child, file_path, source, locals, enclosing, in_function_body);
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
        ..Default::default()
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
        ..Default::default()
    }
}
