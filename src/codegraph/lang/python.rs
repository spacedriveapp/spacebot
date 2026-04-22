//! Python language provider.

use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider};
use super::languages::SupportedLanguage;
use crate::codegraph::semantic::member_rules;
use crate::codegraph::types::NodeLabel;

pub struct PythonProvider;

impl LanguageProvider for PythonProvider {
    fn language(&self) -> SupportedLanguage {
        SupportedLanguage::Python
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
            NodeLabel::Class,
            NodeLabel::Function,
            NodeLabel::Method,
            NodeLabel::Variable,
            NodeLabel::Decorator,
            NodeLabel::Import,
        ]
    }

    fn queries(&self) -> Option<&'static super::queries::QuerySet> {
        Some(&super::queries::python::QUERY_SET)
    }
}

#[cfg(feature = "codegraph")]
fn extract_with_tree_sitter(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return extract_fallback(file_path, content);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return extract_fallback(file_path, content),
    };

    let mut symbols = Vec::new();
    walk_py_node(tree.root_node(), file_path, content, &mut symbols, None);
    symbols
}

#[cfg(feature = "codegraph")]
fn walk_py_node(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
    parent_name: Option<&str>,
) {
    let kind = node.kind();
    match kind {
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                let qname = qname(file_path, parent_name, &name);
                let extends = node
                    .child_by_field_name("superclasses")
                    .and_then(|n| n.child(0))
                    .map(|n| n.utf8_text(source.as_bytes()).unwrap_or("").to_string());
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    qualified_name: qname.clone(),
                    label: NodeLabel::Class,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    parent: parent_name.map(String::from),
                    import_source: None,
                    extends,
                    implements: Vec::new(),
                    decorates: None,
                    metadata: std::collections::HashMap::new(),
                    is_exported: member_rules::is_exported(
                        SupportedLanguage::Python,
                        member_rules::classify_visibility(
                            SupportedLanguage::Python,
                            &[],
                            name.as_ref(),
                        ),
                        &[],
                        name.as_ref(),
                    ),
                    ..Default::default()
                });
                if let Some(body) = node.child_by_field_name("body") {
                    // Collect class-level assignments (`x = 0`, `x: T = 0`)
                    // and instance assignments (`self.x = ...`) into one
                    // deduplicated map keyed by field name.
                    let mut fields: std::collections::HashMap<String, (u32, u32, Option<String>)> =
                        std::collections::HashMap::new();
                    collect_py_class_fields(body, source, &mut fields);
                    for (fname, (line_start, line_end, declared_type)) in &fields {
                        let mut metadata = std::collections::HashMap::new();
                        if let Some(ty) = declared_type
                            && !ty.is_empty()
                        {
                            metadata.insert("declared_type".to_string(), ty.clone());
                        }
                        symbols.push(ExtractedSymbol {
                            name: fname.clone(),
                            qualified_name: format!("{qname}::{fname}"),
                            label: NodeLabel::Variable,
                            line_start: *line_start,
                            line_end: *line_end,
                            parent: Some(qname.clone()),
                            import_source: None,
                            extends: None,
                            implements: Vec::new(),
                            decorates: None,
                            metadata,
                            ..Default::default()
                        });
                    }

                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_py_node(child, file_path, source, symbols, Some(&qname));
                    }
                }
                return;
            }
        }
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                let label = if parent_name.is_some() {
                    NodeLabel::Method
                } else {
                    NodeLabel::Function
                };
                let fn_qname = qname(file_path, parent_name, &name);
                let mut metadata = std::collections::HashMap::new();
                let mut ret_type = None;
                if let Some(return_type) = node.child_by_field_name("return_type") {
                    let ty = text_str(return_type, source);
                    if !ty.is_empty() {
                        metadata.insert("declared_type".to_string(), ty.clone());
                        ret_type = Some(ty);
                    }
                }
                let param_count = node.child_by_field_name("parameters").map(|params| {
                    let cursor = &mut params.walk();
                    params.children(cursor).filter(|c| {
                        matches!(c.kind(), "identifier" | "typed_parameter" | "default_parameter"
                            | "typed_default_parameter" | "list_splat_pattern" | "dictionary_splat_pattern")
                    }).filter(|c| {
                        let t = c.child_by_field_name("name")
                            .map(|n| text_str(n, source))
                            .or_else(|| if c.kind() == "identifier" { Some(text_str(*c, source)) } else { None })
                            .unwrap_or_default();
                        t != "self" && t != "cls"
                    }).count() as u32
                });
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
                    is_exported: member_rules::is_exported(
                        SupportedLanguage::Python,
                        member_rules::classify_visibility(
                            SupportedLanguage::Python,
                            &[],
                            name.as_ref(),
                        ),
                        &[],
                        name.as_ref(),
                    ),
                    return_type: ret_type,
                    parameter_count: param_count,
                    ..Default::default()
                });

                if let Some(params) = node.child_by_field_name("parameters") {
                    collect_py_params(params, source, &fn_qname, symbols);
                }
            }
        }
        "import_statement" | "import_from_statement" => {
            collect_py_imports(node, file_path, source, symbols);
        }
        "decorated_definition" => {
            // Process the decorators, then recurse into the definition.
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                if child.kind() == "decorator" {
                    let dec_text = child.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                    symbols.push(ExtractedSymbol {
                        name: dec_text.trim_start_matches('@').to_string(),
                        qualified_name: format!("{file_path}::decorator::{dec_text}"),
                        label: NodeLabel::Decorator,
                        line_start: child.start_position().row as u32 + 1,
                        line_end: child.end_position().row as u32 + 1,
                        parent: parent_name.map(String::from),
                        import_source: None,
                        extends: None,
                        implements: Vec::new(),
                        decorates: None,
                        metadata: std::collections::HashMap::new(),
                        ..Default::default()
                    });
                } else {
                    walk_py_node(child, file_path, source, symbols, parent_name);
                }
            }
            return;
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_py_node(child, file_path, source, symbols, parent_name);
    }
}

/// Emit one Import node per imported binding from a Python
/// `import_statement` or `import_from_statement`.
///
/// Layouts handled:
/// - `import foo`                 → name="foo", source="foo"
/// - `import foo as bar`          → name="bar", source="foo", original_name="foo"
/// - `import foo, bar`            → two nodes
/// - `from foo import bar`        → name="bar", source="foo"
/// - `from foo import bar as baz` → name="baz", source="foo", original_name="bar"
/// - `from foo import *`          → name="*", source="foo"
/// - `from . import foo`          → name="foo", source="." (relative)
#[cfg(feature = "codegraph")]
fn collect_py_imports(
    stmt: tree_sitter::Node,
    file_path: &str,
    source: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let line_start = stmt.start_position().row as u32 + 1;
    let line_end = stmt.end_position().row as u32 + 1;
    let kind = stmt.kind();

    let push = |name: &str,
                import_source: &str,
                original: Option<&str>,
                symbols: &mut Vec<ExtractedSymbol>| {
        let qname = if name.is_empty() {
            format!("{file_path}::import::{import_source}")
        } else {
            format!("{file_path}::import::{import_source}::{name}")
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
            parent: None,
            import_source: Some(import_source.to_string()),
            extends: None,
            implements: Vec::new(),
            decorates: None,
            metadata,
            ..Default::default()
        });
    };

    if kind == "import_statement" {
        // `import a, b as c, d.e.f` — each `name:` field is either a
        // dotted_name (bound locally to its leftmost segment) or an
        // aliased_import (name: dotted_name, alias: identifier).
        let cursor = &mut stmt.walk();
        for child in stmt.children(cursor) {
            match child.kind() {
                "dotted_name" => {
                    let module = text_str(child, source);
                    // `import foo.bar` binds `foo` in the local namespace.
                    let local = module.split('.').next().unwrap_or(&module).to_string();
                    if !local.is_empty() {
                        push(&local, &module, None, symbols);
                    }
                }
                "aliased_import" => {
                    let module = child
                        .child_by_field_name("name")
                        .map(|n| text_str(n, source))
                        .unwrap_or_default();
                    let alias = child
                        .child_by_field_name("alias")
                        .map(|n| text_str(n, source))
                        .unwrap_or_default();
                    if !alias.is_empty() {
                        let orig = if alias != module { Some(module.as_str()) } else { None };
                        push(&alias, &module, orig, symbols);
                    }
                }
                _ => {}
            }
        }
        return;
    }

    // import_from_statement: one module_name, any number of `name:` children.
    let module_name = stmt
        .child_by_field_name("module_name")
        .map(|n| text_str(n, source))
        .unwrap_or_default();

    let mut emitted = false;
    let cursor = &mut stmt.walk();
    for child in stmt.children(cursor) {
        match child.kind() {
            "wildcard_import" => {
                push("*", &module_name, None, symbols);
                emitted = true;
            }
            "dotted_name" => {
                // Skip the module_name dotted_name — it's a field child
                // and will be the same node. Only non-module children
                // represent imported names.
                if let Some(module_node) = stmt.child_by_field_name("module_name")
                    && module_node.id() == child.id()
                {
                    continue;
                }
                let name = text_str(child, source);
                if !name.is_empty() {
                    let local = name.split('.').next().unwrap_or(&name).to_string();
                    push(&local, &module_name, None, symbols);
                    emitted = true;
                }
            }
            "aliased_import" => {
                let orig_name = child
                    .child_by_field_name("name")
                    .map(|n| text_str(n, source))
                    .unwrap_or_default();
                let alias = child
                    .child_by_field_name("alias")
                    .map(|n| text_str(n, source))
                    .unwrap_or_default();
                if !alias.is_empty() {
                    let orig = if alias != orig_name {
                        Some(orig_name.as_str())
                    } else {
                        None
                    };
                    push(&alias, &module_name, orig, symbols);
                    emitted = true;
                }
            }
            _ => {}
        }
    }

    if !emitted && !module_name.is_empty() {
        push("", &module_name, None, symbols);
    }
}

/// Collect function/method parameters as Parameter symbols parented
/// to the enclosing function. Tree-sitter-python wraps parameters in
/// a `parameters` node containing children of several kinds:
/// - `identifier` for plain `x`
/// - `typed_parameter` for `x: int`
/// - `default_parameter` for `x = 5`
/// - `typed_default_parameter` for `x: int = 5`
/// - `list_splat_pattern` / `dictionary_splat_pattern` for `*args` / `**kwargs`
///
/// Skips `self` and `cls` since they're not real arguments to the
/// function — they're the receiver, already represented by the
/// containing class.
#[cfg(feature = "codegraph")]
fn collect_py_params(
    params_node: tree_sitter::Node,
    source: &str,
    function_qname: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let cursor = &mut params_node.walk();
    for child in params_node.children(cursor) {
        let (pname, declared_type) = match child.kind() {
            "identifier" => (Some(text_str(child, source)), None),
            "typed_parameter" | "typed_default_parameter" => {
                // The parameter name sometimes lives on the `name` field
                // and sometimes only as the first identifier child —
                // which varies by tree-sitter-python grammar version.
                let name = child
                    .child_by_field_name("name")
                    .map(|n| text_str(n, source))
                    .or_else(|| {
                        let cur = &mut child.walk();
                        for c in child.children(cur) {
                            if c.kind() == "identifier" {
                                return Some(text_str(c, source));
                            }
                        }
                        None
                    });
                let ty = child
                    .child_by_field_name("type")
                    .map(|n| text_str(n, source));
                (name, ty)
            }
            "default_parameter" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| text_str(n, source))
                    .or_else(|| {
                        let cur = &mut child.walk();
                        for c in child.children(cur) {
                            if c.kind() == "identifier" {
                                return Some(text_str(c, source));
                            }
                        }
                        None
                    });
                (name, None)
            }
            "list_splat_pattern" | "dictionary_splat_pattern" => {
                // *args / **kwargs — strip the splat marker.
                let cur = &mut child.walk();
                let mut found = None;
                for c in child.children(cur) {
                    if c.kind() == "identifier" {
                        found = Some(text_str(c, source));
                        break;
                    }
                }
                (found, None)
            }
            _ => (None, None),
        };

        let Some(pname) = pname else { continue };
        if pname.is_empty() || pname == "self" || pname == "cls" {
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
            ..Default::default()
        });
    }
}

#[cfg(feature = "codegraph")]
fn text_str(node: tree_sitter::Node, source: &str) -> String {
    node.utf8_text(source.as_bytes()).unwrap_or("").to_string()
}

/// Collect all class field names from a class body, including:
/// - class-level assignments: `x = 0` or `x: int = 0`
/// - instance assignments inside methods: `self.x = ...`
///
/// Returned map keys are field names; values are
/// `(start_line, end_line, declared_type)` of the first occurrence.
/// Nested classes are not descended into so their fields don't bleed
/// into the parent.
#[cfg(feature = "codegraph")]
fn collect_py_class_fields(
    node: tree_sitter::Node,
    source: &str,
    fields: &mut std::collections::HashMap<String, (u32, u32, Option<String>)>,
) {
    let kind = node.kind();

    // Don't descend into nested class bodies — those fields belong to
    // the inner class, which gets its own walk_py_node pass.
    if kind == "class_definition" {
        return;
    }

    if kind == "assignment"
        && let Some(left) = node.child_by_field_name("left")
    {
        // Annotated assignments (`x: Foo = ...`) carry a `type`
        // field on the assignment node itself.
        let declared_type = node
            .child_by_field_name("type")
            .map(|n| text_str(n, source));

        match left.kind() {
            // `x = 0`  or  `x: int = 0`  at class level
            "identifier" => {
                let fname = left
                    .utf8_text(source.as_bytes())
                    .unwrap_or("")
                    .to_string();
                if !fname.is_empty() {
                    fields.entry(fname).or_insert((
                        node.start_position().row as u32 + 1,
                        node.end_position().row as u32 + 1,
                        declared_type,
                    ));
                }
            }
            // `self.x = ...` inside a method
            "attribute" => {
                if let Some(object) = left.child_by_field_name("object")
                    && object.utf8_text(source.as_bytes()).unwrap_or("") == "self"
                    && let Some(attr) = left.child_by_field_name("attribute")
                {
                    let fname = attr
                        .utf8_text(source.as_bytes())
                        .unwrap_or("")
                        .to_string();
                    if !fname.is_empty() {
                        fields.entry(fname).or_insert((
                            node.start_position().row as u32 + 1,
                            node.end_position().row as u32 + 1,
                            declared_type,
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        collect_py_class_fields(child, source, fields);
    }
}

#[cfg(feature = "codegraph")]
fn extract_calls_tree_sitter(file_path: &str, content: &str) -> Vec<CallSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    walk_py_calls(tree.root_node(), file_path, content, &mut calls, &mut Vec::new());
    calls
}

#[cfg(feature = "codegraph")]
fn walk_py_calls(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    calls: &mut Vec<CallSite>,
    enclosing: &mut Vec<String>,
) {
    let kind = node.kind();

    match kind {
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source.as_bytes()).unwrap_or("");
                let cq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(cq);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_py_calls(child, file_path, source, calls, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source.as_bytes()).unwrap_or("");
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_py_calls(child, file_path, source, calls, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "decorated_definition" => {
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                walk_py_calls(child, file_path, source, calls, enclosing);
            }
            return;
        }
        "call" => {
            if let Some(caller) = enclosing.last()
                && let Some(func_node) = node.child_by_field_name("function")
            {
                match func_node.kind() {
                    "identifier" => {
                        let name = func_node.utf8_text(source.as_bytes()).unwrap_or("");
                        if !name.is_empty() {
                            calls.push(CallSite {
                                caller_qualified_name: caller.clone(),
                                callee_name: name.to_string(),
                                line: node.start_position().row as u32 + 1,
                                is_method_call: false,
                                receiver: None,
                            });
                        }
                    }
                    "attribute" => {
                        let method = func_node
                            .child_by_field_name("attribute")
                            .and_then(|n| {
                                let t = n.utf8_text(source.as_bytes()).unwrap_or("");
                                if t.is_empty() { None } else { Some(t.to_string()) }
                            });
                        let recv = func_node
                            .child_by_field_name("object")
                            .map(|n| n.utf8_text(source.as_bytes()).unwrap_or("").to_string());
                        if let Some(name) = method {
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
            // Fall through to recurse into nested calls like foo(bar())
        }
        _ => {}
    }

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        walk_py_calls(child, file_path, source, calls, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_accesses_tree_sitter(file_path: &str, content: &str) -> Vec<AccessSite> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut accesses = Vec::new();
    walk_py_accesses(
        tree.root_node(),
        file_path,
        content,
        &mut accesses,
        &mut Vec::new(),
    );
    accesses
}

/// Walk the AST collecting `self.X` field accesses inside function/method
/// bodies. Mirrors `walk_py_calls` but emits `AccessSite` records instead.
#[cfg(feature = "codegraph")]
fn walk_py_accesses(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    accesses: &mut Vec<AccessSite>,
    enclosing: &mut Vec<String>,
) {
    let kind = node.kind();

    match kind {
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source.as_bytes()).unwrap_or("");
                let cq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(cq);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_py_accesses(child, file_path, source, accesses, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source.as_bytes()).unwrap_or("");
                let fq = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(fq);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_py_accesses(child, file_path, source, accesses, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "decorated_definition" => {
            let cursor = &mut node.walk();
            for child in node.children(cursor) {
                walk_py_accesses(child, file_path, source, accesses, enclosing);
            }
            return;
        }
        "attribute" => {
            // Emit when receiver is bare `self` and we're inside a function.
            // Chained accesses like `self.x.y` recurse — we only emit the
            // outermost `self.<name>`, since `y` lives on a different type.
            if let Some(caller) = enclosing.last()
                && let Some(object) = node.child_by_field_name("object")
                && object.kind() == "identifier"
                && object.utf8_text(source.as_bytes()).unwrap_or("") == "self"
                && let Some(attr) = node.child_by_field_name("attribute")
            {
                let field = attr
                    .utf8_text(source.as_bytes())
                    .unwrap_or("")
                    .to_string();
                if !field.is_empty() {
                    accesses.push(AccessSite {
                        caller_qualified_name: caller.clone(),
                        field_name: field,
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
        walk_py_accesses(child, file_path, source, accesses, enclosing);
    }
}

#[cfg(feature = "codegraph")]
fn extract_tests_tree_sitter(file_path: &str, content: &str) -> Vec<String> {
    use tree_sitter::Parser;

    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut tests = Vec::new();
    walk_py_tests(
        tree.root_node(),
        file_path,
        content,
        &mut tests,
        &mut Vec::new(),
    );
    tests
}

#[cfg(feature = "codegraph")]
fn walk_py_tests(
    node: tree_sitter::Node,
    file_path: &str,
    source: &str,
    tests: &mut Vec<String>,
    enclosing: &mut Vec<String>,
) {
    match node.kind() {
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text_str(name_node, source);
                let outer = match enclosing.last() {
                    Some(p) => format!("{p}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                enclosing.push(outer);
                if let Some(body) = node.child_by_field_name("body") {
                    let cursor = &mut body.walk();
                    for child in body.children(cursor) {
                        walk_py_tests(child, file_path, source, tests, enclosing);
                    }
                }
                enclosing.pop();
                return;
            }
        }
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text_str(name_node, source);
                // Pytest and unittest both look for `test_`-prefixed
                // methods; matching on name alone skips fixtures and
                // helpers without false positives.
                if name.starts_with("test_") || name == "test" {
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
        walk_py_tests(child, file_path, source, tests, enclosing);
    }
}

fn extract_fallback(file_path: &str, content: &str) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = i as u32 + 1;
        if let Some(rest) = trimmed.strip_prefix("class ") {
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                symbols.push(sym(file_path, &name, NodeLabel::Class, line_num));
            }
        } else if let Some(rest) = trimmed.strip_prefix("def ") {
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                symbols.push(sym(file_path, &name, NodeLabel::Function, line_num));
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

fn sym(file_path: &str, name: &str, label: NodeLabel, line: u32) -> ExtractedSymbol {
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
        is_exported: member_rules::is_exported(
                        SupportedLanguage::Python,
                        member_rules::classify_visibility(SupportedLanguage::Python, &[], name),
                        &[],
                        name,
                    ),
        ..Default::default()
    }
}
