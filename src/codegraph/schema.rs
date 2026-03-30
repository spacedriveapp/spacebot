//! Graph schema definitions and DDL for LadybugDB.
//!
//! LadybugDB uses Cypher. Node tables and relationship tables must be created
//! before data can be inserted. This module produces the DDL statements.

/// Current schema version. Bump this when making breaking changes.
pub const SCHEMA_VERSION: u32 = 1;

/// Generate all DDL statements for the code graph schema.
///
/// Returns a vector of Cypher DDL strings that should be executed in order.
pub fn schema_ddl() -> Vec<String> {
    let mut ddl = Vec::new();

    // -----------------------------------------------------------------------
    // Node tables
    // -----------------------------------------------------------------------

    ddl.push(node_table(
        "Project",
        &[
            ("qualified_name", "STRING"),
            ("name", "STRING"),
            ("project_id", "STRING"),
            ("source", "STRING"),
            ("root_path", "STRING"),
        ],
    ));

    for label in &[
        "Package", "Module", "Folder", "File", "Class", "Function", "Method",
        "Variable", "Interface", "Enum", "Decorator", "Import", "Type",
        "Struct", "Macro", "Trait", "Impl", "Namespace", "TypeAlias", "Const",
        "Record", "Template", "Test",
    ] {
        ddl.push(node_table(
            label,
            &[
                ("qualified_name", "STRING"),
                ("name", "STRING"),
                ("project_id", "STRING"),
                ("source_file", "STRING"),
                ("line_start", "INT32"),
                ("line_end", "INT32"),
                ("source", "STRING"),
                ("written_by", "STRING"),
            ],
        ));
    }

    // Semantic nodes with extra fields.
    ddl.push(node_table(
        "Community",
        &[
            ("qualified_name", "STRING"),
            ("name", "STRING"),
            ("project_id", "STRING"),
            ("description", "STRING"),
            ("node_count", "INT64"),
            ("file_count", "INT64"),
            ("function_count", "INT64"),
            ("density", "DOUBLE"),
            ("source", "STRING"),
        ],
    ));

    ddl.push(node_table(
        "Process",
        &[
            ("qualified_name", "STRING"),
            ("name", "STRING"),
            ("project_id", "STRING"),
            ("entry_function", "STRING"),
            ("source_file", "STRING"),
            ("call_depth", "INT32"),
            ("source", "STRING"),
        ],
    ));

    ddl.push(node_table(
        "Section",
        &[
            ("qualified_name", "STRING"),
            ("name", "STRING"),
            ("project_id", "STRING"),
            ("source_file", "STRING"),
            ("heading_level", "INT32"),
            ("content", "STRING"),
            ("source", "STRING"),
        ],
    ));

    // -----------------------------------------------------------------------
    // Relationship tables
    // -----------------------------------------------------------------------

    // All code-entity node tables share the same schema, so we define
    // relationships generically. LadybugDB (KuzuDB) requires FROM/TO to
    // reference specific node table pairs. We'll create the most common
    // combinations; the full cross-product can be extended later.
    let entity_tables: &[&str] = &[
        "Project", "Package", "Module", "Folder", "File", "Class", "Function",
        "Method", "Variable", "Interface", "Enum", "Decorator", "Import",
        "Type", "Struct", "Macro", "Trait", "Impl", "Namespace", "TypeAlias",
        "Const", "Record", "Template", "Community", "Process", "Section",
        "Test",
    ];

    // CONTAINS: structural containment (parent -> child)
    for &parent in &["Project", "Package", "Module", "Folder", "File", "Class", "Namespace"] {
        for &child in entity_tables {
            if parent != child {
                ddl.push(rel_table("CONTAINS", parent, child, &[("project_id", "STRING")]));
            }
        }
    }

    // DEFINES: File -> Symbol
    for &symbol in &[
        "Class", "Function", "Method", "Variable", "Interface", "Enum",
        "Decorator", "Type", "Struct", "Macro", "Trait", "Impl", "Namespace",
        "TypeAlias", "Const", "Record", "Template", "Test",
    ] {
        ddl.push(rel_table("DEFINES", "File", symbol, &[("project_id", "STRING")]));
    }

    // CALLS: caller -> callee (with confidence)
    for &caller in &["Function", "Method"] {
        for &callee in &["Function", "Method"] {
            ddl.push(rel_table(
                "CALLS",
                caller,
                callee,
                &[("confidence", "DOUBLE"), ("project_id", "STRING"), ("source", "STRING")],
            ));
        }
    }

    // IMPORTS: File -> Symbol
    for &symbol in entity_tables {
        ddl.push(rel_table("IMPORTS", "File", symbol, &[("project_id", "STRING")]));
    }

    // Heritage: EXTENDS, IMPLEMENTS, INHERITS, OVERRIDES
    for &edge in &["EXTENDS", "IMPLEMENTS", "INHERITS"] {
        for &from in &["Class", "Interface", "Struct", "Trait"] {
            for &to in &["Class", "Interface", "Struct", "Trait"] {
                ddl.push(rel_table(edge, from, to, &[("project_id", "STRING")]));
            }
        }
    }

    ddl.push(rel_table("OVERRIDES", "Method", "Method", &[("project_id", "STRING")]));

    // HAS_METHOD, HAS_PROPERTY
    for &owner in &["Class", "Interface", "Struct", "Trait", "Impl"] {
        ddl.push(rel_table("HAS_METHOD", owner, "Method", &[("project_id", "STRING")]));
        ddl.push(rel_table("HAS_PROPERTY", owner, "Variable", &[("project_id", "STRING")]));
    }

    // ACCESSES: Function/Method -> Variable
    for &accessor in &["Function", "Method"] {
        ddl.push(rel_table("ACCESSES", accessor, "Variable", &[("project_id", "STRING")]));
    }

    // USES: generic
    for &from in entity_tables {
        for &to in entity_tables {
            if from != to {
                ddl.push(rel_table("USES", from, to, &[("project_id", "STRING")]));
            }
        }
    }

    // DECORATES
    for &target in &["Class", "Function", "Method", "Variable"] {
        ddl.push(rel_table("DECORATES", "Decorator", target, &[("project_id", "STRING")]));
    }

    // MEMBER_OF: Symbol -> Community
    for &symbol in entity_tables {
        if symbol != "Community" {
            ddl.push(rel_table("MEMBER_OF", symbol, "Community", &[("project_id", "STRING")]));
        }
    }

    // STEP_IN_PROCESS: Process step sequencing
    ddl.push(rel_table(
        "STEP_IN_PROCESS",
        "Process",
        "Function",
        &[("step_order", "INT32"), ("project_id", "STRING")],
    ));
    ddl.push(rel_table(
        "STEP_IN_PROCESS",
        "Process",
        "Method",
        &[("step_order", "INT32"), ("project_id", "STRING")],
    ));

    // TESTED_BY: Function/Method -> Test
    for &testee in &["Function", "Method"] {
        ddl.push(rel_table("TESTED_BY", testee, "Test", &[("project_id", "STRING")]));
    }

    ddl
}

/// Generate a CREATE NODE TABLE statement.
fn node_table(label: &str, columns: &[(&str, &str)]) -> String {
    let cols: Vec<String> = columns
        .iter()
        .map(|(name, ty)| format!("{name} {ty}"))
        .collect();
    format!(
        "CREATE NODE TABLE IF NOT EXISTS {label} (id SERIAL, {cols}, PRIMARY KEY(id))",
        label = label,
        cols = cols.join(", "),
    )
}

/// Generate a CREATE REL TABLE statement.
fn rel_table(name: &str, from: &str, to: &str, props: &[(&str, &str)]) -> String {
    let prop_str = if props.is_empty() {
        String::new()
    } else {
        let p: Vec<String> = props
            .iter()
            .map(|(name, ty)| format!("{name} {ty}"))
            .collect();
        format!(", {}", p.join(", "))
    };
    format!(
        "CREATE REL TABLE IF NOT EXISTS {name}_{from}_{to} (FROM {from} TO {to}{prop_str})",
    )
}
