//! Graph schema definitions and DDL for LadybugDB.
//!
//! LadybugDB uses Cypher. Node tables and relationship tables must be created
//! before data can be inserted. This module produces the DDL statements.
//!
//! Uses a single `CodeRelation` table with a `type` property rather than
//! per-edge-type tables (e.g. `CONTAINS_Folder_File`). This keeps schema
//! init fast (~30 DDL statements instead of ~1000) and simplifies all
//! Cypher queries to use `:CodeRelation {type: 'CALLS', ...}`.

/// Current schema version. Bump this when node or relationship table
/// columns change. `ensure_schema` compares against the version stored
/// in the DB; on mismatch it drops every table and recreates so the
/// new columns are available.
///
/// v12 landed the consolidated Phase 5+8 schema additions in one
/// bump: Middleware nodes (route-wrapping), SqlQuery / CicsCall /
/// DliCall nodes (COBOL EXEC blocks, Phase 7), JclJob / JclStep
/// nodes (JCL parser, Phase 7), plus the relationship pairs those
/// node types need. Pre-adding everything here avoids four
/// consecutive reindex cycles across the remaining phases.
pub const SCHEMA_VERSION: u32 = 12;

/// All node table labels. Used by the pipeline to purge stale data before re-indexing.
pub const ALL_NODE_LABELS: &[&str] = &[
    "Project", "Package", "Module", "Folder", "File", "Class", "Function",
    "Method", "Variable", "Parameter", "Interface", "Enum", "Decorator", "Import", "Type",
    "Struct", "MacroDef", "Trait", "Impl", "Namespace", "TypeAlias", "Const",
    "Record", "Template", "Test", "Community", "Process", "Section", "Route", "Tool",
    "Property", "Constructor", "Typedef", "UnionType", "Static", "Delegate", "CodeElement",
    // Phase 5 framework-layer: HTTP middleware wrapping routes.
    "Middleware",
    // Phase 7 mainframe: SQL/CICS/DLI embedded calls + JCL jobs.
    "SqlQuery", "CicsCall", "DliCall", "JclJob", "JclStep",
    "CodeEmbedding",
];

/// Node labels for display, stats, and graph queries. Excludes the
/// pipeline-only labels that are created for resolution and deleted
/// before finalization.
pub const DISPLAY_NODE_LABELS: &[&str] = &[
    "Project", "Package", "Module", "Folder", "File", "Class", "Function",
    "Method", "Interface", "Enum", "Type", "Variable", "Import", "Decorator",
    "Struct", "MacroDef", "Trait", "Impl", "Namespace", "TypeAlias", "Const",
    "Record", "Template", "Test", "Community", "Process", "Section", "Route", "Tool",
    "Property", "Constructor", "Typedef", "UnionType", "Static", "Delegate", "CodeElement",
    "Middleware",
    "SqlQuery", "CicsCall", "DliCall", "JclJob", "JclStep",
];

/// Labels that exist only during pipeline execution and are deleted before
/// the final graph is committed. Parameters are needed to resolve argument
/// types against callee signatures but would flood the graph with per-call
/// noise, so they're purged after call resolution.
pub const PIPELINE_ONLY_LABELS: &[&str] = &["Parameter"];

/// Generate DROP statements for all tables so the schema can be rebuilt
/// from scratch when the version changes. Rel table must be dropped
/// before node tables because LadybugDB rejects dropping a node table
/// that has active relationships.
pub fn schema_drop_ddl() -> Vec<String> {
    let mut ddl = Vec::new();

    ddl.push("DROP TABLE IF EXISTS CodeRelation".to_string());

    for label in ALL_NODE_LABELS {
        ddl.push(format!("DROP TABLE IF EXISTS {label}"));
    }
    ddl.push("DROP TABLE IF EXISTS _SchemaVersion".to_string());

    ddl
}

/// Generate all DDL statements for the code graph schema.
///
/// Returns a vector of Cypher DDL strings that should be executed in order.
pub fn schema_ddl() -> Vec<String> {
    let mut ddl = Vec::new();

    // Sentinel table that stores the schema version. ensure_schema reads
    // this on startup and drops+recreates when it doesn't match the code.
    ddl.push(
        "CREATE NODE TABLE IF NOT EXISTS _SchemaVersion (\
         id SERIAL, version INT32, PRIMARY KEY(id))"
            .to_string(),
    );
    ddl.push(format!(
        "CREATE (n:_SchemaVersion {{version: {SCHEMA_VERSION}}})"
    ));

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
        "Variable", "Parameter", "Interface", "Enum", "Decorator", "Import", "Type",
        "Struct", "MacroDef", "Trait", "Impl", "Namespace", "TypeAlias", "Const",
        "Record", "Template", "Test", "Route", "Section", "Tool",
        "Property", "Constructor", "Typedef", "UnionType", "Static", "Delegate", "CodeElement",
        // Phase 5/7 additions — same column shape so queries that
        // project generic `source_file` / `line_start` / etc. work
        // uniformly across every symbol-shaped node.
        "Middleware", "SqlQuery", "CicsCall", "DliCall", "JclJob", "JclStep",
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
                ("declared_type", "STRING"),
                ("content", "STRING"),
                ("description", "STRING"),
                // GitNexus-parity symbol metadata
                ("is_exported", "BOOL"),
                ("return_type", "STRING"),
                ("visibility", "STRING"),
                ("parameter_count", "INT32"),
                ("is_static", "BOOL"),
                ("is_readonly", "BOOL"),
                ("is_abstract", "BOOL"),
                ("is_final", "BOOL"),
                ("annotations", "STRING"),
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
            ("cohesion", "DOUBLE"),
            ("keywords", "STRING"),
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
            ("process_type", "STRING"),
            ("communities", "STRING"),
            ("entry_point_score", "DOUBLE"),
            ("terminal_id", "STRING"),
            ("source", "STRING"),
        ],
    ));

    // -----------------------------------------------------------------------
    // CodeEmbedding table for vector search (384-dim, all-MiniLM-L6-v2)
    // -----------------------------------------------------------------------
    ddl.push(
        "CREATE NODE TABLE IF NOT EXISTS CodeEmbedding (\
         nodeId STRING, \
         embedding FLOAT[384], \
         PRIMARY KEY (nodeId))"
            .to_string(),
    );

    // -----------------------------------------------------------------------
    // Single CodeRelation table
    // -----------------------------------------------------------------------
    // LadybugDB requires explicit FROM/TO node-type pairs. We enumerate the
    // valid combinations rather than doing the full Cartesian product.

    let structural = &["Project", "Folder"];
    let containers = &["File", "Class", "Interface", "Struct", "Trait", "Impl",
                        "Enum", "Module", "Namespace", "Package", "UnionType"];
    let symbols = &["Function", "Method", "Variable", "Class", "Interface",
                     "Enum", "Struct", "Trait", "Impl", "MacroDef", "TypeAlias",
                     "Const", "Decorator", "Import", "Type", "Record",
                     "Template", "Namespace", "Module", "Test",
                     "Property", "Constructor", "Typedef", "UnionType", "Static",
                     "Delegate", "CodeElement"];
    let callable = &["Function", "Method", "Constructor"];
    let inheritable = &["Class", "Interface", "Struct", "Trait"];
    let owners = &["Class", "Interface", "Struct", "Trait", "Impl", "UnionType"];

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

    // Heritage: EXTENDS, IMPLEMENTS
    for &a in inheritable {
        for &b in inheritable {
            pairs.push((a, b));
        }
    }

    // OVERRIDES: Method → Method
    pairs.push(("Method", "Method"));

    // HAS_METHOD: owner → Method/Constructor, HAS_PROPERTY: owner → Variable/Property
    for &o in owners {
        pairs.push((o, "Method"));
        pairs.push((o, "Constructor"));
        pairs.push((o, "Variable"));
        pairs.push((o, "Property"));
    }

    // ACCESSES: callable → Variable/Property
    for &c in callable {
        pairs.push((c, "Variable"));
        pairs.push((c, "Property"));
    }

    // HAS_PARAMETER: callable → Parameter (and File → Parameter for DEFINES)
    for &c in callable {
        pairs.push((c, "Parameter"));
    }
    pairs.push(("File", "Parameter"));

    // DECORATES: Decorator → targets
    for &t in &["Class", "Function", "Method", "Variable", "Constructor", "Property"] {
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

    // HANDLES_ROUTE: callable → Route
    for &c in callable {
        pairs.push((c, "Route"));
    }

    // HANDLES_TOOL: callable → Tool
    for &c in callable {
        pairs.push((c, "Tool"));
    }

    // WRAPS: Middleware → Route (middleware tracing for framework stacks)
    pairs.push(("Middleware", "Route"));

    // FETCHES: File → Route (template / JS file calls into a
    // Route's URL; used by both the JS/TS fetch-call extractor and
    // HTML form-action detection).
    pairs.push(("File", "Route"));

    // QUERIES: callable → SqlQuery / CicsCall / DliCall (COBOL EXEC blocks)
    for &c in callable {
        pairs.push((c, "SqlQuery"));
        pairs.push((c, "CicsCall"));
        pairs.push((c, "DliCall"));
    }

    // JCL topology: Job → Step → File (DD-statement cross-references)
    pairs.push(("JclJob", "JclStep"));
    pairs.push(("JclStep", "File"));
    // JclStep → callable, to link step-executed programs to their
    // COBOL entry points once the JCL processor resolves PGM=
    // references.
    for &c in callable {
        pairs.push(("JclStep", c));
    }

    // ENTRY_POINT_OF: callable → Process
    for &c in callable {
        pairs.push((c, "Process"));
    }

    // Type-resolution edges: callable/Variable/Parameter → inheritable
    // (receiver-type CALLS, declared_type lookups)
    for &src in &["Function", "Method", "Variable", "Parameter"] {
        for &tgt in inheritable {
            pairs.push((src, tgt));
        }
    }

    // CONTAINS: File → Section, Section → Section (markdown headings)
    pairs.push(("File", "Section"));
    pairs.push(("Section", "Section"));

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
