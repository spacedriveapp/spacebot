//! TypeScript/JavaScript language provider.
//!
//! Extracts Class, Function, Method, Variable, Interface, Enum, Type,
//! Decorator, Namespace, TypeAlias, and Const nodes from TS/JS files
//! using tree-sitter.

use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider};
use super::languages::SupportedLanguage;
use crate::codegraph::types::NodeLabel;

pub struct TypeScriptProvider;

impl LanguageProvider for TypeScriptProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::TypeScript
    }

    fn extract_symbols(&self, file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
        #[cfg(feature = "codegraph")]
        {
            extract_with_tree_sitter(file_path, content, true)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            extract_fallback(file_path, content)
        }
    }

    fn extract_calls(&self, file_path: &str, content: &str) -> Vec<CallSite> {
        #[cfg(feature = "codegraph")]
        {
            extract_calls_tree_sitter(file_path, content, true)
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
            extract_accesses_tree_sitter(file_path, content, true)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            let _ = (file_path, content);
            Vec::new()
        }
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Class,
            NodeLabel::Function,
            NodeLabel::Method,
            NodeLabel::Variable,
            NodeLabel::Interface,
            NodeLabel::Enum,
            NodeLabel::Decorator,
            NodeLabel::Import,
            NodeLabel::Type,
            NodeLabel::Namespace,
            NodeLabel::TypeAlias,
            NodeLabel::Const,
        ]
    }
}

pub struct JavaScriptProvider;

impl LanguageProvider for JavaScriptProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::JavaScript
    }

    fn extract_symbols(&self, file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
        #[cfg(feature = "codegraph")]
        {
            extract_with_tree_sitter(file_path, content, false)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            extract_fallback(file_path, content)
        }
    }

    fn extract_calls(&self, file_path: &str, content: &str) -> Vec<CallSite> {
        #[cfg(feature = "codegraph")]
        {
            extract_calls_tree_sitter(file_path, content, false)
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
            extract_accesses_tree_sitter(file_path, content, false)
        }
        #[cfg(not(feature = "codegraph"))]
        {
            let _ = (file_path, content);
            Vec::new()
        }
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Class,
            NodeLabel::Function,
            NodeLabel::Method,
            NodeLabel::Variable,
            NodeLabel::Decorator,
            NodeLabel::Import,
        ]
    }
}

/// Extract symbols using tree-sitter with the TypeScript or JavaScript grammar.
#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(
    file_path: &str,
    content: &str,
    is_typescript: bool,
) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language = if is_typescript {
        if file_path.ends_with(".tsx") {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    } else {
        tree_sitter_javascript::LANGUAGE.into()
    };

    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        tracing::warn!(file = file_path, "failed to set tree-sitter language");
        return extract_fallback(file_path, content);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => {
            tracing::warn!(file = file_path, "tree-sitter parse returned None");
            return extract_fallback(file_path, content);
        }
    };

    let mut symbols = Vec::new();
    let root = tree.root_node();

    // Walk the tree manually for reliability across grammar versions.
    walk_ts_node(root, file_path, content, &mut symbols, None);

    symbols
}

#[cfg(feature = "codegraph")]
fn walk_ts_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    let kind = node.kind();

    match kind {
        "class_declaration" | "class" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let qname = qualified_name(file_path, parent_name, &name);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname.clone(),
                    label: NodeLabel::Class,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends: child_text_by_field(node, "superclass", source),
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
                // Recurse into class body with class as parent.
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_children(body, file_path, source, symbols, Some(&qname));
                }
                return; // Don't recurse into children again.
            }
        }
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let fn_qname = qualified_name(file_path, parent_name, &name);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: fn_qname.clone(),
                    label: NodeLabel::Function,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
                if let Some(params) = node.child_by_field_name("parameters") {
                    collect_ts_params(params, source, &fn_qname, symbols);
                }
            }
        }
        "method_definition" | "public_field_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let label = if kind == "method_definition" {
                    NodeLabel::Method
                } else {
                    NodeLabel::Variable
                };
                let fn_qname = qualified_name(file_path, parent_name, &name);
                // public_field_definition may carry a type annotation
                // (`foo: Bar = ...`) that's the field's type.
                let mut metadata = std::collections::HashMap::new();
                if kind == "public_field_definition"
                    && let Some(type_node) = node.child_by_field_name("type")
                {
                    let ty = clean_ts_type_annotation(&node_text(type_node, source));
                    if !ty.is_empty() {
                        metadata.insert("declared_type".to_string(), ty);
                    }
                }
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: fn_qname.clone(),
                    label,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata,
                });
                // Methods (and only methods) get parameter extraction.
                if kind == "method_definition"
                    && let Some(params) = node.child_by_field_name("parameters")
                {
                    collect_ts_params(params, source, &fn_qname, symbols);
                }
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let qname = qualified_name(file_path, parent_name, &name);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname.clone(),
                    label: NodeLabel::Interface,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
                // Recurse into interface body to extract property/method signatures.
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_children(body, file_path, source, symbols, Some(&qname));
                }
                return;
            }
        }
        // Interface/class property and method signatures.
        "property_signature" | "property_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                // Capture the annotation type for interface/class
                // property declarations (`foo: Bar`).
                let mut metadata = std::collections::HashMap::new();
                if let Some(type_node) = node.child_by_field_name("type") {
                    let ty = clean_ts_type_annotation(&node_text(type_node, source));
                    if !ty.is_empty() {
                        metadata.insert("declared_type".to_string(), ty);
                    }
                }
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qualified_name(file_path, parent_name, &name),
                    label: NodeLabel::Variable,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata,
                });
            }
        }
        "method_signature" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qualified_name(file_path, parent_name, &name),
                    label: NodeLabel::Method,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
            }
        }
        "abstract_class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let qname = qualified_name(file_path, parent_name, &name);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname.clone(),
                    label: NodeLabel::Class,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends: child_text_by_field(node, "superclass", source),
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_children(body, file_path, source, symbols, Some(&qname));
                }
                return;
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let qname = qualified_name(file_path, parent_name, &name);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname.clone(),
                    label: NodeLabel::Enum,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
                // Recurse into enum body to extract members.
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_children(body, file_path, source, symbols, Some(&qname));
                }
                return;
            }
        }
        "type_alias_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qualified_name(file_path, parent_name, &name),
                    label: NodeLabel::TypeAlias,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
            }
        }
        "import_statement" => {
            if let Some(source_node) = node.child_by_field_name("source") {
                let source_text = node_text(source_node, source)
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                symbols.push(ExtractedSymbol {
                    name: source_text.clone(),
                    qualified_name: format!("{file_path}::import::{source_text}"),
                    label: NodeLabel::Import,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: None,
                    import_source: Some(source_text),
                    extends: None,
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                });
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            // Handle `const foo = () => {}` and `var foo = ...`.
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if child.kind() == "variable_declarator"
                    && let Some(name_node) = child.child_by_field_name("name")
                {
                    let value_kind = child
                        .child_by_field_name("value")
                        .map(|v| v.kind().to_string());
                    let label = match value_kind.as_deref() {
                        Some("arrow_function") | Some("function") => NodeLabel::Function,
                        _ => NodeLabel::Variable,
                    };
                    let name = node_text(name_node, source);
                    symbols.push(ExtractedSymbol {
                        name: name.clone(),
                        qualified_name: qualified_name(file_path, parent_name, &name),
                        label,
                        line_start: node.start_position().row as u32 + 1,
                        line_end: node.end_position().row as u32 + 1,
                        parent: parent_name.map(String::from),
                        import_source: None,
                        extends: None,
                        implements: Vec::new(),
                        decorates: None,
                        metadata: std::collections::HashMap::new(),
                    });
                }
            }
        }
        "enum_body" => {
            // Extract individual enum members.
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if (child.kind() == "enum_member" || child.kind() == "property_identifier")
                    && let Some(name_node) = child.child_by_field_name("name")
                {
                    let name = node_text(name_node, source);
                    symbols.push(ExtractedSymbol {
                        name: name.clone(),
                        qualified_name: qualified_name(file_path, parent_name, &name),
                        label: NodeLabel::Variable,
                        line_start: child.start_position().row as u32 + 1,
                        line_end: child.end_position().row as u32 + 1,
                        parent: parent_name.map(String::from),
                        import_source: None,
                        extends: None,
                        implements: Vec::new(),
                        decorates: None,
                        metadata: std::collections::HashMap::new(),
                    });
                }
            }
        }
        "export_statement" => {
            // Recurse into the exported declaration so `export function...` etc. are extracted.
            walk_ts_children(node, file_path, source, symbols, parent_name);
            return; // Already recursed
        }
        _ => {}
    }

    // Recurse into children.
    walk_ts_children(node, file_path, source, symbols, parent_name);
}

#[cfg(feature = "codegraph")]
fn walk_ts_children(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_ts_node(child, file_path, source, symbols, parent_name);
    }
}

/// Collect TS/JS function/method parameters as Parameter symbols.
/// Tree-sitter-typescript wraps params in `formal_parameters` containing:
/// - `required_parameter` / `optional_parameter` for `x: T` / `x?: T`
/// - `rest_parameter` for `...args`
/// - `identifier` (in some JS contexts) for plain `x`
///
/// Each typed parameter has a `pattern` field that's an identifier
/// (or destructuring — we skip destructured params for now).
#[cfg(feature = "codegraph")]
fn collect_ts_params(
    params_node: tree_sitter::Node,
    source: &str,
    function_qname: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let cursor = &mut params_node.walk();
    for child in params_node.children(cursor) {
        let (pname, declared_type) = match child.kind() {
            "identifier" => (Some(node_text(child, source)), None),
            "required_parameter" | "optional_parameter" | "rest_parameter" => {
                let name = child
                    .child_by_field_name("pattern")
                    .filter(|n| n.kind() == "identifier")
                    .map(|n| node_text(n, source));
                // The `type` field is a `type_annotation` node whose
                // text includes the leading `:`/`?:` — strip it.
                let ty = child
                    .child_by_field_name("type")
                    .map(|n| clean_ts_type_annotation(&node_text(n, source)));
                (name, ty)
            }
            _ => (None, None),
        };

        let Some(pname) = pname else { continue };
        if pname.is_empty() || pname == "this" {
            continue;
        }

        let mut metadata = std::collections::HashMap::new();
        if let Some(ty) = declared_type
            && !ty.is_empty()
        {
            metadata.insert("declared_type".to_string(), ty);
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

/// Strip the leading `:` (or `?:`) and surrounding whitespace from a
/// `type_annotation` node's raw text. Tree-sitter-typescript stores the
/// annotation including the colon, so `: Foo` needs to become `Foo`.
#[cfg(feature = "codegraph")]
fn clean_ts_type_annotation(raw: &str) -> String {
    raw.trim_start_matches('?')
        .trim_start_matches(':')
        .trim()
        .to_string()
}

#[cfg(feature = "codegraph")]
fn node_text(node: tree_sitter::Node, source: &str) -> String {
    node.utf8_text(source.as_bytes())
        .unwrap_or("")
        .to_string()
}

#[cfg(feature = "codegraph")]
fn child_text_by_field(
    node: tree_sitter::Node,
    field: &str,
    source: &str,
) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| node_text(n, source))
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(file_path: &str, content: &str, is_typescript: bool) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = if is_typescript {
        if file_path.ends_with(".tsx") {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    } else {
        tree_sitter_javascript::LANGUAGE.into()
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
    walk_ts_calls(tree.root_node(), file_path, content, &mut calls, &mut Vec::new());
    calls
}

#[cfg(feature = "codegraph")]
fn walk_ts_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    let kind = node.kind();

    match kind {
        "class_declaration" | "class" | "abstract_class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let cq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(cq);
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_calls_children(body, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                walk_ts_calls_children(node, file_path, source, calls, enclosing);
                enclosing.pop();
                return;
            }
        }
        "method_definition" | "method_signature" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                walk_ts_calls_children(node, file_path, source, calls, enclosing);
                enclosing.pop();
                return;
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            // Handle `const foo = () => {}` — treat arrow functions as callable scopes.
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if child.kind() == "variable_declarator" {
                    let is_arrow = child
                        .child_by_field_name("value")
                        .map(|v| v.kind() == "arrow_function" || v.kind() == "function")
                        .unwrap_or(false);
                    if is_arrow {
                        if let Some(name_node) = child.child_by_field_name("name") {
                            let name = node_text(name_node, source);
                            let fq = match enclosing.last() {
                                Some(p) => format!("{p}::{name}"),
                                None => format!("{file_path}::{name}"),
                            };
                            enclosing.push(fq);
                            if let Some(val) = child.child_by_field_name("value") {
                                walk_ts_calls_children(val, file_path, source, calls, enclosing);
                            }
                            enclosing.pop();
                        }
                    } else {
                        walk_ts_calls(child, file_path, source, calls, enclosing);
                    }
                }
            }
            return;
        }
        "export_statement" => {
            walk_ts_calls_children(node, file_path, source, calls, enclosing);
            return;
        }
        "call_expression" => {
            if let Some(caller) = enclosing.last()
                && let Some(func_node) = node.child_by_field_name("function")
            {
                match func_node.kind() {
                    "identifier" => {
                        let name = node_text(func_node, source);
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
                    "member_expression" => {
                        let method = func_node
                            .child_by_field_name("property")
                            .map(|n| node_text(n, source));
                        let recv = func_node
                            .child_by_field_name("object")
                            .map(|n| node_text(n, source));
                        if let Some(name) = method
                            && !name.is_empty()
                        {
                            calls.push(CallSite {
                                caller_qualified_name: caller.clone(),
                                callee_name: name,
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
        "new_expression" => {
            if let Some(caller) = enclosing.last()
                && let Some(ctor_node) = node.child_by_field_name("constructor")
            {
                let name = node_text(ctor_node, source);
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

    walk_ts_calls_children(node, file_path, source, calls, enclosing);
}

#[cfg(feature = "codegraph")]
fn walk_ts_calls_children(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_ts_calls(child, file_path, source, calls, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_accesses_tree_sitter(
    file_path: &str,
    content: &str,
    is_typescript: bool,
) -> Vec<AccessSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = if is_typescript {
        if file_path.ends_with(".tsx") {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    } else {
        tree_sitter_javascript::LANGUAGE.into()
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
    walk_ts_accesses(
        tree.root_node(),
        file_path,
        content,
        &mut accesses,
        &mut Vec::new(),
    );
    accesses
}

/// Walk the TS/JS AST collecting `this.field` accesses inside method bodies.
/// Mirrors `walk_ts_calls` enclosing-stack pattern.
#[cfg(feature = "codegraph")]
fn walk_ts_accesses(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    accesses: &mut Vec<AccessSite>,
    enclosing: &mut Vec<String>,
) {
    let kind = node.kind();

    match kind {
        "class_declaration" | "class" | "abstract_class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let cq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(cq);
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_accesses_children(body, file_path, source, accesses, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                walk_ts_accesses_children(node, file_path, source, accesses, enclosing);
                enclosing.pop();
                return;
            }
        }
        "method_definition" | "method_signature" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                walk_ts_accesses_children(node, file_path, source, accesses, enclosing);
                enclosing.pop();
                return;
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            // Track `const foo = () => {}` arrow functions as callable scopes.
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if child.kind() == "variable_declarator" {
                    let is_arrow = child
                        .child_by_field_name("value")
                        .map(|v| v.kind() == "arrow_function" || v.kind() == "function")
                        .unwrap_or(false);
                    if is_arrow
                        && let Some(name_node) = child.child_by_field_name("name")
                    {
                        let name = node_text(name_node, source);
                        let fq = match enclosing.last() {
                            Some(p) => format!("{p}::{name}"),
                            None => format!("{file_path}::{name}"),
                        };
                        enclosing.push(fq);
                        if let Some(val) = child.child_by_field_name("value") {
                            walk_ts_accesses_children(val, file_path, source, accesses, enclosing);
                        }
                        enclosing.pop();
                    } else {
                        walk_ts_accesses(child, file_path, source, accesses, enclosing);
                    }
                }
            }
            return;
        }
        "export_statement" => {
            walk_ts_accesses_children(node, file_path, source, accesses, enclosing);
            return;
        }
        "member_expression" => {
            // `this.field` — emit access only when object is bare `this`.
            if let Some(caller) = enclosing.last()
                && let Some(object) = node.child_by_field_name("object")
                && object.kind() == "this"
                && let Some(prop) = node.child_by_field_name("property")
            {
                let fname = node_text(prop, source);
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

    walk_ts_accesses_children(node, file_path, source, accesses, enclosing);
}

#[cfg(feature = "codegraph")]
fn walk_ts_accesses_children(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    accesses: &mut Vec<AccessSite>,
    enclosing: &mut Vec<String>,
) {
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_ts_accesses(child, file_path, source, accesses, enclosing);
    }
}

/// Fallback extractor when tree-sitter is not available (no `codegraph` feature).
fn extract_fallback(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = i as u32 + 1;

        if let Some(name) = extract_simple_pattern(trimmed, "function ") {
            symbols.push(simple_symbol(file_path, &name, NodeLabel::Function, line_num));
        } else if let Some(name) = extract_simple_pattern(trimmed, "class ") {
            symbols.push(simple_symbol(file_path, &name, NodeLabel::Class, line_num));
        } else if let Some(name) = extract_simple_pattern(trimmed, "interface ") {
            symbols.push(simple_symbol(file_path, &name, NodeLabel::Interface, line_num));
        } else if let Some(name) = extract_simple_pattern(trimmed, "enum ") {
            symbols.push(simple_symbol(file_path, &name, NodeLabel::Enum, line_num));
        } else if let Some(name) = extract_simple_pattern(trimmed, "type ")
            && (name.contains('=') || name.contains('<'))
        {
            let clean = name.split(['=', '<']).next().unwrap_or(&name).trim();
            symbols.push(simple_symbol(file_path, clean, NodeLabel::TypeAlias, line_num));
        }
    }
    symbols
}

fn extract_simple_pattern(line: &str, prefix: &str) -> Option<String> {
    if !line.starts_with(prefix) && !line.starts_with(&format!("export {prefix}")) {
        return None;
    }
    let after = if line.starts_with("export ") {
        &line[("export ".len() + prefix.len())..]
    } else {
        &line[prefix.len()..]
    };
    let name: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(feature = "codegraph")]
fn qualified_name(file_path: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}::{name}"),
        None => format!("{file_path}::{name}"),
    }
}

fn simple_symbol(
    file_path: &str,
    name: &str,
    label: NodeLabel,
    line: u32,
) -> ExtractedSymbol {
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
