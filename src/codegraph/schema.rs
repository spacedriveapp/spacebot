//! Graph schema definitions and DDL for LadybugDB.
//!
//! LadybugDB uses Cypher. Node tables and relationship tables must be created
//! before data can be inserted. This module produces the DDL statements.
//!
//! Uses a single `CodeRelation` table with a `type` property rather than
//! per-edge-type tables (e.g. `CONTAINS_Folder_File`). This keeps schema
//! init fast (~30 DDL statements instead of ~1000) and simplifies all
//! Cypher queries to use `:CodeRelation {type: 'CALLS', ...}`.

/// Current schema version. Bump this when making breaking changes.
pub const SCHEMA_VERSION: u32 = 2;

/// All node table labels. Used by the pipeline to purge stale data before re-indexing.
pub const ALL_NODE_LABELS: &[&str] = &[
    "Project", "Package", "Module", "Folder", "File", "Class", "Function",
    "Method", "Variable", "Interface", "Enum", "Decorator", "Import", "Type",
    "Struct", "MacroDef", "Trait", "Impl", "Namespace", "TypeAlias", "Const",
    "Record", "Template", "Test", "Community", "Process", "Section",
];

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
        "Struct", "MacroDef", "Trait", "Impl", "Namespace", "TypeAlias", "Const",
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
                ("extends_type", "STRING"),
                ("import_source", "STRING"),
            ],
        ));
    }

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
    // Single CodeRelation table
    // -----------------------------------------------------------------------
    // LadybugDB requires explicit FROM/TO node-type pairs. We enumerate the
    // valid combinations rather than doing the full Cartesian product.

    let structural = &["Project", "Folder"];
    let containers = &["File", "Class", "Interface", "Struct", "Trait", "Impl",
                        "Enum", "Module", "Namespace", "Package"];
    let symbols = &["Function", "Method", "Variable", "Class", "Interface",
                     "Enum", "Struct", "Trait", "Impl", "MacroDef", "TypeAlias",
                     "Const", "Decorator", "Import", "Type", "Record",
                     "Template", "Namespace", "Module", "Test"];
    let callable = &["Function", "Method"];
    let inheritable = &["Class", "Interface", "Struct", "Trait"];
    let owners = &["Class", "Interface", "Struct", "Trait", "Impl"];

    let mut pairs: Vec<(&str, &str)> = Vec::new();

    // CONTAINS: structural → folders/files, containers → symbols
    for &p in structural {
        pairs.push((p, "Folder"));
        pairs.push((p, "File"));
    }
    for &c in containers {
        for &s in symbols {
            pairs.push((c, s));
        }
    }

    // DEFINES: File → any symbol
    for &s in symbols {
        pairs.push(("File", s));
    }

    // CALLS: callable → callable
    for &a in callable {
        for &b in callable {
            pairs.push((a, b));
        }
    }

    // IMPORTS: File → File (cross-file import)
    pairs.push(("File", "File"));

    // Heritage: EXTENDS, IMPLEMENTS, INHERITS
    for &a in inheritable {
        for &b in inheritable {
            pairs.push((a, b));
        }
    }

    // OVERRIDES: Method → Method
    pairs.push(("Method", "Method"));

    // HAS_METHOD: owner → Method, HAS_PROPERTY: owner → Variable
    for &o in owners {
        pairs.push((o, "Method"));
        pairs.push((o, "Variable"));
    }

    // ACCESSES: callable → Variable
    for &c in callable {
        pairs.push((c, "Variable"));
    }

    // DECORATES: Decorator → targets
    for &t in &["Class", "Function", "Method", "Variable"] {
        pairs.push(("Decorator", t));
    }

    // MEMBER_OF: any symbol → Community
    for &s in symbols {
        pairs.push((s, "Community"));
    }
    pairs.push(("File", "Community"));

    // STEP_IN_PROCESS: Process → callable
    for &c in callable {
        pairs.push(("Process", c));
    }

    // TESTED_BY: callable → Test
    for &c in callable {
        pairs.push((c, "Test"));
    }

    // Deduplicate pairs (some overlap from the loops above)
    pairs.sort();
    pairs.dedup();

    // Build the single CodeRelation DDL
    let from_to_clauses: Vec<String> = pairs
        .iter()
        .map(|(f, t)| format!("FROM {f} TO {t}"))
        .collect();

    ddl.push(format!(
        "CREATE REL TABLE IF NOT EXISTS CodeRelation ({}, \
         type STRING, confidence DOUBLE, reason STRING, step INT32)",
        from_to_clauses.join(", "),
    ));

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
