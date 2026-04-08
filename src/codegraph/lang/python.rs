//! Python language provider.

use super::provider::{AccessSite, CallSite, ExtractedSymbol, LanguageProvider};
use super::languages::SupportedLanguage;
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
                });
                if let Some(body) = node.child_by_field_name("body") {
                    // Collect class field names (class-level assignments and
                    // self.X = ... assignments inside methods), deduped.
                    let mut fields: std::collections::HashMap<String, (u32, u32)> =
                        std::collections::HashMap::new();
                    collect_py_class_fields(body, source, &mut fields);
                    for (fname, (line_start, line_end)) in &fields {
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
                            metadata: std::collections::HashMap::new(),
                        });
                    }

                    // Then recurse normally for nested classes / methods.
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
                    metadata: std::collections::HashMap::new(),
                });

                // Extract parameters as Parameter nodes parented to the function.
                if let Some(params) = node.child_by_field_name("parameters") {
                    collect_py_params(params, source, &fn_qname, symbols);
                }
            }
        }
        "import_statement" | "import_from_statement" => {
            let text = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            let source_module = if kind == "import_from_statement" {
                node.child_by_field_name("module_name")
                    .map(|n| n.utf8_text(source.as_bytes()).unwrap_or("").to_string())
            } else {
                Some(text.trim_start_matches("import ").trim().to_string())
            };
            symbols.push(ExtractedSymbol {
                name: source_module.clone().unwrap_or_default(),
                qualified_name: format!("{file_path}::import::{}", source_module.as_deref().unwrap_or("?")),
                label: NodeLabel::Import,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                parent: None,
                import_source: source_module,
                extends: None,
                implements: Vec::new(),
                decorates: None,
                metadata: std::collections::HashMap::new(),
            });
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
        let pname = match child.kind() {
            "identifier" => Some(text_str(child, source)),
            "typed_parameter" | "default_parameter" | "typed_default_parameter" => {
                // Name lives on the first identifier child or in the
                // `name` field depending on grammar version. Try field
                // first, fall back to scanning children.
                child
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
                    })
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
                found
            }
            _ => None,
        };

        let Some(pname) = pname else { continue };
        if pname.is_empty() || pname == "self" || pname == "cls" {
            continue;
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
            metadata: std::collections::HashMap::new(),
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
/// Returned map keys are field names; values are (start_line, end_line)
/// of the first occurrence. Nested classes are not descended into so
/// their fields don't bleed into the parent.
#[cfg(feature = "codegraph")]
fn collect_py_class_fields(
    node: tree_sitter::Node,
    source: &str,
    fields: &mut std::collections::HashMap<String, (u32, u32)>,
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
    }
}
