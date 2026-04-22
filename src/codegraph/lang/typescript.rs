//! TypeScript/JavaScript language provider.
//!
//! Extracts Class, Function, Method, Variable, Interface, Enum, Type,
//! Decorator, Namespace, TypeAlias, and Const nodes from TS/JS files
//! using tree-sitter.

use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider, LocalBinding};
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

    fn queries(&self) -> Option<&'static super::queries::QuerySet> {
        Some(&super::queries::typescript::QUERY_SET)
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

    fn queries(&self) -> Option<&'static super::queries::QuerySet> {
        Some(&super::queries::javascript::QUERY_SET)
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
    walk_ts_node(root, file_path, content, &mut symbols, None, false);

    symbols
}

/// Check whether a tree-sitter node has a child token with the given kind
/// (e.g. `"static"`, `"readonly"`, `"abstract"`, `"override"`).
#[cfg(feature = "codegraph")]
fn has_modifier(node: tree_sitter::Node, modifier: &str) -> bool {
    // tree-sitter-typescript represents modifiers as child nodes whose
    // `kind()` matches the keyword text (e.g. "static", "abstract",
    // "override"). Just walk direct children and compare kinds.
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        if child.kind() == modifier {
            return true;
        }
    }
    false
}

/// Extract the accessibility modifier ("public", "private", "protected") from
/// a method_definition or property node.
#[cfg(feature = "codegraph")]
fn extract_visibility(node: tree_sitter::Node, source: &str) -> Option<String> {
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        match child.kind() {
            "accessibility_modifier" | "public" | "private" | "protected" => {
                let text = node_text(child, source);
                match text.as_str() {
                    "public" | "private" | "protected" => return Some(text),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    None
}

/// Count the number of parameters in a `formal_parameters` node.
#[cfg(feature = "codegraph")]
fn count_parameters(params_node: tree_sitter::Node) -> u32 {
    let cursor = &mut params_node.walk();
    let mut count = 0u32;
    for child in params_node.children(cursor) {
        match child.kind() {
            "required_parameter" | "optional_parameter" | "rest_parameter" | "identifier" => {
                count += 1;
            }
            _ => {}
        }
    }
    count
}

/// Extract return type from a function/method node's `return_type` field.
#[cfg(feature = "codegraph")]
fn extract_return_type(node: tree_sitter::Node, source: &str) -> Option<String> {
    node.child_by_field_name("return_type")
        .map(|n| clean_ts_type_annotation(&node_text(n, source)))
        .filter(|s| !s.is_empty())
}

/// Collect decorator names from sibling `decorator` nodes that appear
/// immediately before the given declaration node. In tree-sitter-typescript,
/// decorators are siblings of the decorated declaration inside the parent.
#[cfg(feature = "codegraph")]
fn collect_decorators(node: tree_sitter::Node, source: &str) -> Vec<String> {
    let mut decorators = Vec::new();
    // Walk preceding siblings looking for decorator nodes.
    let mut sibling = node.prev_sibling();
    while let Some(sib) = sibling {
        if sib.kind() == "decorator" {
            // The decorator node contains an expression (identifier or call_expression).
            // Extract just the name.
            let text = node_text(sib, source);
            let name = text
                .trim_start_matches('@')
                .split('(')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !name.is_empty() {
                decorators.push(name);
            }
            sibling = sib.prev_sibling();
        } else {
            break;
        }
    }
    decorators.reverse();
    decorators
}

/// Check whether a node has `readonly` among its children.
#[cfg(feature = "codegraph")]
fn has_readonly(node: tree_sitter::Node, source: &str) -> bool {
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        if child.kind() == "readonly" {
            return true;
        }
        // Some grammar versions use a generic modifier token.
        if node_text(child, source) == "readonly" {
            return true;
        }
    }
    false
}

#[cfg(feature = "codegraph")]
fn walk_ts_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
    is_exported: bool,
) {
    let kind = node.kind();

    match kind {
        "class_declaration" | "class" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let qname = qualified_name(file_path, parent_name, &name);
                let decorators = collect_decorators(node, source);
                let annotations = if decorators.is_empty() {
                    None
                } else {
                    Some(decorators.join(","))
                };
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname.clone(),
                    label: NodeLabel::Class,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    extends: child_text_by_field(node, "superclass", source),
                    is_exported,
                    annotations,
                    ..Default::default()
                });
                // Recurse into class body with class as parent.
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_children(body, file_path, source, symbols, Some(&qname), false);
                }
                return; // Don't recurse into children again.
            }
        }
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let fn_qname = qualified_name(file_path, parent_name, &name);
                let mut metadata = std::collections::HashMap::new();
                let return_type = extract_return_type(node, source);
                if let Some(ref ty) = return_type {
                    metadata.insert("declared_type".to_string(), ty.clone());
                }
                let parameter_count = node
                    .child_by_field_name("parameters")
                    .map(|p| count_parameters(p));
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: fn_qname.clone(),
                    label: NodeLabel::Function,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    metadata,
                    is_exported,
                    return_type,
                    parameter_count,
                    ..Default::default()
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
                // Both public_field_definition (`foo: Bar = ...`) and
                // method_definition (`foo(): Bar`) can expose a type —
                // the former through the `type` field and the latter
                // through `return_type`. Try both so each kind is
                // covered regardless of grammar quirks across versions.
                let mut metadata = std::collections::HashMap::new();
                if let Some(type_node) = node.child_by_field_name("type") {
                    let ty = clean_ts_type_annotation(&node_text(type_node, source));
                    if !ty.is_empty() {
                        metadata.insert("declared_type".to_string(), ty);
                    }
                }
                let mut return_type = None;
                if kind == "method_definition"
                    && let Some(rt_node) = node.child_by_field_name("return_type")
                {
                    let ty = clean_ts_type_annotation(&node_text(rt_node, source));
                    if !ty.is_empty() {
                        metadata.insert("declared_type".to_string(), ty.clone());
                        return_type = Some(ty);
                    }
                }
                let visibility = extract_visibility(node, source);
                let is_static = has_modifier(node, "static");
                let is_readonly = has_readonly(node, source);
                let is_abstract = has_modifier(node, "abstract");
                let parameter_count = if kind == "method_definition" {
                    node.child_by_field_name("parameters")
                        .map(|p| count_parameters(p))
                } else {
                    None
                };
                let decorators = collect_decorators(node, source);
                let annotations = if decorators.is_empty() {
                    None
                } else {
                    Some(decorators.join(","))
                };
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: fn_qname.clone(),
                    label,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    metadata,
                    visibility,
                    is_static,
                    is_readonly,
                    is_abstract,
                    return_type,
                    parameter_count,
                    annotations,
                    ..Default::default()
                });
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
                    is_exported,
                    ..Default::default()
                });
                // Recurse into interface body to extract property/method signatures.
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_children(body, file_path, source, symbols, Some(&qname), false);
                }
                return;
            }
        }
        "property_signature" | "property_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let mut metadata = std::collections::HashMap::new();
                if let Some(type_node) = node.child_by_field_name("type") {
                    let ty = clean_ts_type_annotation(&node_text(type_node, source));
                    if !ty.is_empty() {
                        metadata.insert("declared_type".to_string(), ty);
                    }
                }
                let is_readonly = has_readonly(node, source);
                let is_static = has_modifier(node, "static");
                let visibility = extract_visibility(node, source);
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qualified_name(file_path, parent_name, &name),
                    label: NodeLabel::Variable,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    metadata,
                    is_readonly,
                    is_static,
                    visibility,
                    ..Default::default()
                });
            }
        }
        "method_signature" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let return_type = extract_return_type(node, source);
                let parameter_count = node
                    .child_by_field_name("parameters")
                    .map(|p| count_parameters(p));
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qualified_name(file_path, parent_name, &name),
                    label: NodeLabel::Method,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    return_type,
                    parameter_count,
                    ..Default::default()
                });
            }
        }
        "abstract_class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let qname = qualified_name(file_path, parent_name, &name);
                let decorators = collect_decorators(node, source);
                let annotations = if decorators.is_empty() {
                    None
                } else {
                    Some(decorators.join(","))
                };
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname.clone(),
                    label: NodeLabel::Class,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    extends: child_text_by_field(node, "superclass", source),
                    is_exported,
                    is_abstract: true,
                    annotations,
                    ..Default::default()
                });
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_children(body, file_path, source, symbols, Some(&qname), false);
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
                    is_exported,
                    ..Default::default()
                });
                // Recurse into enum body to extract members.
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_children(body, file_path, source, symbols, Some(&qname), false);
                }
                return;
            }
        }
        "type_alias_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                // Generic type declarations (`type Result<T, E> = ...`)
                // get the Type label; plain aliases use TypeAlias.
                let has_type_params = node
                    .child_by_field_name("type_parameters")
                    .is_some();
                let label = if has_type_params {
                    NodeLabel::Type
                } else {
                    NodeLabel::TypeAlias
                };
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qualified_name(file_path, parent_name, &name),
                    label,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    is_exported,
                    ..Default::default()
                });
            }
        }
        "import_statement" => {
            if let Some(source_node) = node.child_by_field_name("source") {
                let source_text = node_text(source_node, source)
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                collect_ts_imports(node, file_path, &source_text, source, symbols);
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            // Handle `const foo = () => {}` and `var foo = ...`.
            // Check if `const` (readonly for variable bindings).
            let is_const = kind == "lexical_declaration" && {
                let cursor = &mut node.walk();
                node.children(cursor).any(|c| c.kind() == "const")
            };
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if child.kind() == "variable_declarator"
                    && let Some(name_node) = child.child_by_field_name("name")
                {
                    let value_node = child.child_by_field_name("value");
                    let value_kind = value_node.map(|v| v.kind().to_string());
                    let is_arrow_or_fn = matches!(
                        value_kind.as_deref(),
                        Some("arrow_function") | Some("function")
                    );
                    let label = if is_arrow_or_fn {
                        NodeLabel::Function
                    } else {
                        NodeLabel::Variable
                    };
                    let name = node_text(name_node, source);
                    // For arrow/function values, extract return type and parameter count.
                    let (return_type, parameter_count) = if is_arrow_or_fn {
                        let val = value_node.unwrap();
                        let rt = extract_return_type(val, source);
                        let pc = val
                            .child_by_field_name("parameters")
                            .map(|p| count_parameters(p));
                        (rt, pc)
                    } else {
                        (None, None)
                    };
                    symbols.push(ExtractedSymbol {
                        name: name.clone(),
                        qualified_name: qualified_name(file_path, parent_name, &name),
                        label,
                        line_start: node.start_position().row as u32 + 1,
                        line_end: node.end_position().row as u32 + 1,
                        parent: parent_name.map(String::from),
                        is_exported,
                        is_readonly: is_const,
                        return_type,
                        parameter_count,
                        ..Default::default()
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
                        ..Default::default()
                    });
                }
            }
        }
        "export_statement" => {
            // Recurse into the exported declaration so `export function...` etc. are extracted.
            walk_ts_children(node, file_path, source, symbols, parent_name, true);
            return; // Already recursed
        }
        _ => {}
    }

    // Recurse into children.
    walk_ts_children(node, file_path, source, symbols, parent_name, is_exported);
}

#[cfg(feature = "codegraph")]
fn walk_ts_children(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
    is_exported: bool,
) {
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_ts_node(child, file_path, source, symbols, parent_name, is_exported);
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
            metadata,
            ..Default::default()
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

/// Emit one Import node per imported binding from an `import_statement`.
/// Default / namespace / named-imports each contribute one or more nodes;
/// side-effect-only imports (`import "./style.css"`) emit a single nameless
/// node so the file-level `File → File` edge still resolves.
#[cfg(feature = "codegraph")]
fn collect_ts_imports(
    stmt: tree_sitter::Node,
    file_path: &str,
    source_text: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let line_start = stmt.start_position().row as u32 + 1;
    let line_end = stmt.end_position().row as u32 + 1;

    let push = |name: &str, original: Option<&str>, symbols: &mut Vec<ExtractedSymbol>| {
        let qname = if name.is_empty() {
            format!("{file_path}::import::{source_text}")
        } else {
            format!("{file_path}::import::{source_text}::{name}")
        };
        let mut metadata = std::collections::HashMap::new();
        if let Some(orig) = original {
            metadata.insert("original_name".to_string(), orig.to_string());
        }
        symbols.push(ExtractedSymbol {
            name: name.to_string(),
            qualified_name: qname,
            label: NodeLabel::Import,
            line_start,
            line_end,
            import_source: Some(source_text.to_string()),
            metadata,
            ..Default::default()
        });
    };

    let import_clause = {
        let cursor = &mut stmt.walk();
        stmt.children(cursor).find(|c| c.kind() == "import_clause")
    };

    // Side-effect import: no clause, just `import "./style.css"`. Emit
    // one nameless node so the file-level resolver still binds.
    let Some(clause) = import_clause else {
        push("", None, symbols);
        return;
    };

    let mut emitted = false;
    let clause_cursor = &mut clause.walk();
    for child in clause.children(clause_cursor) {
        match child.kind() {
            "identifier" => {
                // Default import: `import Foo from './x'`.
                let name = node_text(child, source);
                if !name.is_empty() {
                    push(&name, None, symbols);
                    emitted = true;
                }
            }
            "namespace_import" => {
                // `import * as foo from './x'` — walk to the identifier.
                let ns_cursor = &mut child.walk();
                for c in child.children(ns_cursor) {
                    if c.kind() == "identifier" {
                        let name = node_text(c, source);
                        if !name.is_empty() {
                            push(&name, None, symbols);
                            emitted = true;
                        }
                        break;
                    }
                }
            }
            "named_imports" => {
                // `import { a, b as c } from './x'` — each import_specifier
                // carries a `name` field and an optional `alias` field.
                let named_cursor = &mut child.walk();
                for spec in child.children(named_cursor) {
                    if spec.kind() != "import_specifier" {
                        continue;
                    }
                    let orig_name = spec
                        .child_by_field_name("name")
                        .map(|n| node_text(n, source))
                        .unwrap_or_default();
                    let local = spec
                        .child_by_field_name("alias")
                        .map(|n| node_text(n, source))
                        .unwrap_or_else(|| orig_name.clone());
                    if !local.is_empty() {
                        let original = if local != orig_name {
                            Some(orig_name.as_str())
                        } else {
                            None
                        };
                        push(&local, original, symbols);
                        emitted = true;
                    }
                }
            }
            _ => {}
        }
    }

    if !emitted {
        push("", None, symbols);
    }
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
            // `new Foo(...)` or `new a.b.Foo(...)` — extract the leaf
            // class name from identifier or member_expression.
            if let Some(caller) = enclosing.last()
                && let Some(ctor) = node.child_by_field_name("constructor")
            {
                let name = match ctor.kind() {
                    "identifier" => node_text(ctor, source),
                    "member_expression" => ctor
                        .child_by_field_name("property")
                        .map(|n| node_text(n, source))
                        .unwrap_or_default(),
                    _ => node_text(ctor, source),
                };
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

#[cfg(feature = "codegraph")]
fn extract_locals_tree_sitter(file_path: &str, content: &str) -> Vec<LocalBinding> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut locals = Vec::new();
    walk_ts_locals(
        tree.root_node(),
        file_path,
        content,
        &mut locals,
        &mut Vec::new(),
    );
    locals
}

#[cfg(feature = "codegraph")]
fn walk_ts_locals(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    locals: &mut Vec<LocalBinding>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class_declaration" | "abstract_class_declaration" | "interface_declaration"
        | "namespace_declaration" | "internal_module" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let outer = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(outer);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_ts_locals(child, file_path, source, locals, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "function_declaration" | "method_definition" | "function_signature" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_ts_locals(child, file_path, source, locals, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            // `const`/`let`/`var` at any nesting level — only emit for
            // bindings that are inside a function and carry an explicit
            // `: Type` annotation on the declarator.
            if let Some(function_qn) = enclosing.last() {
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    if child.kind() != "variable_declarator" {
                        continue;
                    }
                    let Some(pattern) = child.child_by_field_name("name") else {
                        continue;
                    };
                    if pattern.kind() != "identifier" {
                        continue;
                    }
                    let Some(type_node) = child.child_by_field_name("type") else {
                        continue;
                    };
                    let name = node_text(pattern, source);
                    let declared_type = clean_ts_type_annotation(&node_text(type_node, source));
                    if !name.is_empty() && !declared_type.is_empty() {
                        locals.push(LocalBinding {
                            function_qualified_name: function_qn.clone(),
                            name,
                            declared_type,
                        });
                    }
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_ts_locals(child, file_path, source, locals, enclosing);
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
            let has_type_params = name.contains('<');
            let label = if has_type_params { NodeLabel::Type } else { NodeLabel::TypeAlias };
            let clean = name.split(['=', '<']).next().unwrap_or(&name).trim();
            symbols.push(simple_symbol(file_path, clean, label, line_num));
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
        ..Default::default()
    }
}
