//! Java language provider.
//!
//! Extracts:
//! - `class_declaration`       → Class (with `extends` and `implements` lists)
//! - `interface_declaration`   → Interface
//! - `enum_declaration`        → Enum
//! - `method_declaration`      → Method (nested under the owning class/interface)
//! - `constructor_declaration` → Method (name = class name)
//! - `field_declaration`       → Variable
//! - `annotation` / `marker_annotation` → Decorator
//! - `package_declaration`     → Module
//! - `import_declaration`      → Import
//!
//! Call extraction walks `method_invocation` nodes, which always carry a
//! `name` field and optionally an `object` field (for `obj.method()` or
//! `ClassName.method()`).

use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider, LocalBinding};
use super::languages::SupportedLanguage;
use crate::codegraph::types::NodeLabel;

pub struct JavaProvider;

impl LanguageProvider for JavaProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Java
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
        &["java"]
    }

    fn supported_labels(&self) -> &[NodeLabel] {
        &[
            NodeLabel::Class,
            NodeLabel::Interface,
            NodeLabel::Enum,
            NodeLabel::Method,
            NodeLabel::Variable,
            NodeLabel::Decorator,
            NodeLabel::Import,
            NodeLabel::Module,
        ]
    }

    fn queries(&self) -> Option<&'static super::queries::QuerySet> {
        Some(&super::queries::java::QUERY_SET)
    }
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_java_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_java_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    match node.kind() {
        "package_declaration" => {
            if let Some(name_node) = node.child(1) {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, None, &name, NodeLabel::Module, &node, source));
                }
            }
        }
        "import_declaration" => {
            // `import foo.bar.Baz;` — strip the trailing `;` and the leading
            // `import`/`static` keywords. Wildcards keep name="*" and use
            // the package portion as the source.
            let raw = node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .trim_start_matches("import")
                .trim_end_matches(';')
                .trim();
            let path = raw.trim_start_matches("static").trim().to_string();
            if !path.is_empty() {
                let (name, import_source) = if let Some(stripped) = path.strip_suffix(".*") {
                    ("*".to_string(), stripped.to_string())
                } else {
                    let leaf = path.rsplit('.').next().unwrap_or(&path).to_string();
                    (leaf, path.clone())
                };
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
                    metadata: std::collections::HashMap::new(),
                    ..Default::default()
                });
            }
        }
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qname = qname(file_path, parent_name, &name);

                let extends = node
                    .child_by_field_name("superclass")
                    .and_then(|n| n.child_by_field_name("name").or(Some(n)))
                    .map(|n| text(n, source).replace("extends ", "").trim().to_string());

                let implements = collect_implements(node, source);

                let (vis, is_pub, is_static, is_abstract, is_final, annotations) =
                    extract_java_modifiers(node, source.as_bytes());
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname.clone(),
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
                        SupportedLanguage::Java,
                        if is_pub {
                            crate::codegraph::semantic::member_rules::Visibility::Public
                        } else {
                            crate::codegraph::semantic::member_rules::Visibility::Package
                        },
                        if is_pub { &["public"] } else { &[] },
                        name.as_ref(),
                    ),
                    visibility: vis,
                    is_static,
                    is_abstract,
                    is_final,
                    annotations,
                    ..Default::default()
                });

                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_java_node(child, file_path, source, symbols, Some(&qname));
                    }
                }
                return;
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qname = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Interface, &node, source));

                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_java_node(child, file_path, source, symbols, Some(&qname));
                    }
                }
                return;
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let qname = qname(file_path, parent_name, &name);
                symbols.push(sym(file_path, parent_name, &name, NodeLabel::Enum, &node, source));

                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_java_node(child, file_path, source, symbols, Some(&qname));
                    }
                }
                return;
            }
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    let mut method_sym = sym(
                        file_path,
                        parent_name,
                        &name,
                        NodeLabel::Method,
                        &node,
                        source,
                    );
                    if let Some(type_node) = node.child_by_field_name("type") {
                        let ty = text(type_node, source);
                        if !ty.is_empty() && ty != "void" {
                            method_sym
                                .metadata
                                .insert("declared_type".to_string(), ty.clone());
                            method_sym.return_type = Some(ty);
                        }
                    }
                    if let Some(params) = node.child_by_field_name("parameters") {
                        let pc = {
                            let cur = &mut params.walk();
                            params.children(cur)
                                .filter(|c| c.kind() == "formal_parameter" || c.kind() == "spread_parameter")
                                .count() as u32
                        };
                        method_sym.parameter_count = Some(pc);
                    }
                    symbols.push(method_sym);
                    let fn_qname = qname(file_path, parent_name, &name);
                    if let Some(params) = node.child_by_field_name("parameters") {
                        collect_java_params(params, source, &fn_qname, symbols);
                    }
                }
            }
        }
        "constructor_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, parent_name, &name, NodeLabel::Method, &node, source));
                    let fn_qname = qname(file_path, parent_name, &name);
                    if let Some(params) = node.child_by_field_name("parameters") {
                        collect_java_params(params, source, &fn_qname, symbols);
                    }
                }
            }
        }
        "field_declaration" => {
            // A single field_declaration can declare multiple variables
            // that share one type (`private Foo a, b;`), so read the
            // type once at the declaration level and apply it to every
            // variable_declarator child.
            let declared_type = node
                .child_by_field_name("type")
                .map(|n| text(n, source))
                .unwrap_or_default();
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if child.kind() == "variable_declarator"
                    && let Some(name_node) = child.child_by_field_name("name")
                {
                    let name = text(name_node, source);
                    if !name.is_empty() {
                        let mut field_sym = sym(
                            file_path,
                            parent_name,
                            &name,
                            NodeLabel::Variable,
                            &child,
                            source,
                        );
                        if !declared_type.is_empty() {
                            field_sym
                                .metadata
                                .insert("declared_type".to_string(), declared_type.clone());
                        }
                        symbols.push(field_sym);
                    }
                }
            }
        }
        "annotation" | "marker_annotation" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() {
                    symbols.push(sym(file_path, parent_name, &name, NodeLabel::Decorator, &node, source));
                }
            }
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_java_node(child, file_path, source, symbols, parent_name);
    }
}

#[cfg(feature = "codegraph")]
fn collect_implements(class_node: tree_sitter::Node, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(interfaces) = class_node.child_by_field_name("interfaces") {
        let cursor = &mut interfaces.walk();
        for child in interfaces.children(cursor) {
            // child is either `type_list` (Tree-sitter 0.23) or individual types.
            if child.kind() == "type_list" {
                let inner_cursor = &mut child.walk();
                for t in child.children(inner_cursor) {
                    let name = text(t, source);
                    let clean = name.trim_matches(',').trim().to_string();
                    if !clean.is_empty() && clean != "," {
                        out.push(clean);
                    }
                }
            }
        }
    }
    out
}

#[cfg(feature = "codegraph")]
fn text(node: tree_sitter::Node, source: &str) -> String {
    node.utf8_text(source.as_bytes()).unwrap_or("").to_string()
}

/// Collect Java method parameters as Parameter symbols.
/// Tree-sitter-java's `formal_parameters` contains `formal_parameter`
/// children, each with a `name` field.
#[cfg(feature = "codegraph")]
fn collect_java_params(
    params_node: tree_sitter::Node,
    source: &str,
    function_qname: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let cursor = &mut params_node.walk();
    for child in params_node.children(cursor) {
        if child.kind() != "formal_parameter" && child.kind() != "spread_parameter" {
            continue;
        }
        let pname = child
            .child_by_field_name("name")
            .map(|n| text(n, source));
        let Some(pname) = pname else { continue };
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
            ..Default::default()
        });
    }
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(file_path: &str, content: &str) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_java_calls(
        tree.root_node(),
        file_path,
        content,
        &mut calls,
        &mut Vec::new(),
    );
    calls
}

#[cfg(feature = "codegraph")]
fn walk_java_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class_declaration" | "interface_declaration" | "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let outer = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(outer);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_java_calls(child, file_path, source, calls, enclosing);
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
                    walk_java_calls(child, file_path, source, calls, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "method_invocation" => {
            if let Some(caller) = enclosing.last() {
                let name = node
                    .child_by_field_name("name")
                    .map(|n| text(n, source))
                    .unwrap_or_default();
                let receiver = node
                    .child_by_field_name("object")
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
        walk_java_calls(child, file_path, source, calls, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_accesses_tree_sitter(file_path: &str, content: &str) -> Vec<AccessSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut accesses = Vec::new();
    walk_java_accesses(
        tree.root_node(),
        file_path,
        content,
        &mut accesses,
        &mut Vec::new(),
    );
    accesses
}

/// Walk the Java AST collecting `this.field` accesses inside method bodies.
/// Mirrors `walk_java_calls`. Tree-sitter-java uses `field_access` for both
/// `this.x` and `obj.x`; we only emit when the object is the `this` keyword.
#[cfg(feature = "codegraph")]
fn walk_java_accesses(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    accesses: &mut Vec<AccessSite>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class_declaration" | "interface_declaration" | "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let outer = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(outer);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_java_accesses(child, file_path, source, accesses, enclosing);
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
                    walk_java_accesses(child, file_path, source, accesses, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "field_access" => {
            if let Some(caller) = enclosing.last()
                && let Some(object) = node.child_by_field_name("object")
                && object.kind() == "this"
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
        walk_java_accesses(child, file_path, source, accesses, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_locals_tree_sitter(file_path: &str, content: &str) -> Vec<LocalBinding> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut locals = Vec::new();
    walk_java_locals(
        tree.root_node(),
        file_path,
        content,
        &mut locals,
        &mut Vec::new(),
    );
    locals
}

#[cfg(feature = "codegraph")]
fn walk_java_locals(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    locals: &mut Vec<LocalBinding>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class_declaration" | "interface_declaration" | "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let outer = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(outer);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_java_locals(child, file_path, source, locals, enclosing);
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
                    walk_java_locals(child, file_path, source, locals, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "local_variable_declaration" => {
            if let Some(function_qn) = enclosing.last() {
                let declared_type = node
                    .child_by_field_name("type")
                    .map(|n| text(n, source))
                    .unwrap_or_default();
                if !declared_type.is_empty() && declared_type != "var" {
                    let cursor = &mut node.walk();
                    for child in node.children(cursor) {
                        if child.kind() == "variable_declarator"
                            && let Some(name_node) = child.child_by_field_name("name")
                        {
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
        walk_java_locals(child, file_path, source, locals, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_tests_tree_sitter(file_path: &str, content: &str) -> Vec<String> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut tests = Vec::new();
    walk_java_tests(
        tree.root_node(),
        file_path,
        content,
        &mut tests,
        &mut Vec::new(),
    );
    tests
}

#[cfg(feature = "codegraph")]
fn walk_java_tests(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    tests: &mut Vec<String>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class_declaration" | "interface_declaration" | "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                let outer = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(outer);
                let cursor = &mut node.walk();
                for child in node.children(cursor) {
                    walk_java_tests(child, file_path, source, tests, enclosing);
                }
                enclosing.pop();
                return;
            }
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, source);
                if !name.is_empty() && java_method_is_test(node, source) {
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
        walk_java_tests(child, file_path, source, tests, enclosing);
    }
}

/// Look for `@Test`, `@ParameterizedTest`, `@RepeatedTest`, or similar
/// JUnit/TestNG annotations on a `method_declaration`. Annotations live
/// as children of the method's `modifiers` child, not the method itself.
#[cfg(feature = "codegraph")]
fn java_method_is_test(method: tree_sitter::Node, source: &str) -> bool {
    let cursor = &mut method.walk();
    for child in method.children(cursor) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mods_cursor = &mut child.walk();
        for m in child.children(mods_cursor) {
            if !matches!(m.kind(), "annotation" | "marker_annotation") {
                continue;
            }
            let name = m
                .child_by_field_name("name")
                .map(|n| text(n, source))
                .unwrap_or_default();
            let leaf = name.rsplit('.').next().unwrap_or(&name);
            if matches!(
                leaf,
                "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory" | "TestTemplate"
            ) {
                return true;
            }
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
            ("public class ", NodeLabel::Class),
            ("class ", NodeLabel::Class),
            ("public interface ", NodeLabel::Interface),
            ("interface ", NodeLabel::Interface),
            ("public enum ", NodeLabel::Enum),
            ("enum ", NodeLabel::Enum),
            ("package ", NodeLabel::Module),
        ];

        for (prefix, label) in patterns {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
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
    source: &str,
) -> ExtractedSymbol {
    let (vis, is_pub, is_static, is_abstract, is_final, annotations) =
        extract_java_modifiers(*node, source.as_bytes());
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
            SupportedLanguage::Java,
            if is_pub {
                crate::codegraph::semantic::member_rules::Visibility::Public
            } else {
                crate::codegraph::semantic::member_rules::Visibility::Package
            },
            if is_pub { &["public"] } else { &[] },
            name,
        ),
        visibility: vis,
        is_static,
        is_abstract,
        is_final,
        annotations,
        ..Default::default()
    }
}

/// Extract Java modifier info from a declaration node.
/// Returns (visibility, is_public, is_static, is_abstract, is_final, annotations).
#[cfg(feature = "codegraph")]
fn extract_java_modifiers(
    node: tree_sitter::Node,
    source: &[u8],
) -> (Option<String>, bool, bool, bool, bool, Option<String>) {
    let mut vis = None;
    let mut is_pub = false;
    let mut is_static = false;
    let mut is_abstract = false;
    let mut is_final = false;
    let mut anno_names: Vec<String> = Vec::new();

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mods_cursor = &mut child.walk();
        for m in child.children(mods_cursor) {
            let t = m.utf8_text(source).unwrap_or("");
            match t {
                "public" => { vis = Some("public".to_string()); is_pub = true; }
                "private" => { vis = Some("private".to_string()); }
                "protected" => { vis = Some("protected".to_string()); }
                "static" => { is_static = true; }
                "abstract" => { is_abstract = true; }
                "final" => { is_final = true; }
                _ => {
                    if matches!(m.kind(), "annotation" | "marker_annotation")
                        && let Some(name_node) = m.child_by_field_name("name")
                    {
                        let aname = name_node.utf8_text(source).unwrap_or("").to_string();
                        if !aname.is_empty() {
                            anno_names.push(aname);
                        }
                    }
                }
            }
        }
    }
    // Default to package-private if no access modifier found and it's
    // not an annotation/import node.
    if vis.is_none()
        && matches!(
            node.kind(),
            "class_declaration" | "interface_declaration" | "enum_declaration"
                | "method_declaration" | "constructor_declaration" | "field_declaration"
        )
    {
        vis = Some("package-private".to_string());
    }
    let annotations = if anno_names.is_empty() {
        None
    } else {
        Some(anno_names.join(","))
    };
    (vis, is_pub, is_static, is_abstract, is_final, annotations)
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
