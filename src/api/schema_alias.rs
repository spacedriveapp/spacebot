//! GitNexus-name alias layer.
//!
//! Our internal schema uses slightly different label and edge-type
//! names than GitNexus (`MacroDef` vs `Macro`, `UnionType` vs `Union`,
//! no dedicated `Annotation` label). The alias layer is the single
//! place where those names are translated at API boundaries for
//! clients that expect the GitNexus vocabulary.
//!
//! Callers opt in by passing `?schema=gitnexus` on any codegraph
//! response-producing endpoint. When omitted (or `default`), our
//! native names flow through unchanged.

use serde::Deserialize;

/// API vocabulary a caller wants to see.
///
/// `Default` returns our internal schema names verbatim. `GitNexus`
/// translates label and edge-type strings to the GitNexus equivalents
/// via [`translate_label`] / [`translate_edge_type`] before
/// serialization.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum ApiSchema {
    #[default]
    Default,
    GitNexus,
}

/// Translate an internal node-label string into its GitNexus
/// equivalent when `schema` selects GitNexus. Returns the input
/// unchanged otherwise, or when no translation applies.
pub(super) fn translate_label(label: &str, schema: ApiSchema) -> &str {
    if matches!(schema, ApiSchema::Default) {
        return label;
    }
    match label {
        "MacroDef" => "Macro",
        "UnionType" => "Union",
        // `Decorator` is retained as-is — GitNexus accepts both
        // `Decorator` and `Annotation` for the same concept, and
        // `Decorator` is the more widely recognized spelling.
        other => other,
    }
}

/// Translate an internal edge-type string. We currently don't rename
/// any edges, but the helper exists so future schema drift (e.g. if
/// GitNexus adds INHERITS as a first-class distinct from EXTENDS) has
/// a place to go without another API touch-up.
pub(super) fn translate_edge_type(edge_type: &str, _schema: ApiSchema) -> &str {
    // No renames today. See module docs.
    edge_type
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_schema_is_identity() {
        assert_eq!(translate_label("MacroDef", ApiSchema::Default), "MacroDef");
        assert_eq!(translate_label("Function", ApiSchema::Default), "Function");
        assert_eq!(
            translate_edge_type("CALLS", ApiSchema::Default),
            "CALLS"
        );
    }

    #[test]
    fn gitnexus_translates_known_drift() {
        assert_eq!(translate_label("MacroDef", ApiSchema::GitNexus), "Macro");
        assert_eq!(translate_label("UnionType", ApiSchema::GitNexus), "Union");
    }

    #[test]
    fn gitnexus_passes_through_unchanged_labels() {
        for label in ["Function", "Class", "Method", "Interface", "Decorator"] {
            assert_eq!(translate_label(label, ApiSchema::GitNexus), label);
        }
    }

    #[test]
    fn gitnexus_edge_types_are_identity_today() {
        for edge in ["CALLS", "IMPORTS", "EXTENDS", "HAS_METHOD"] {
            assert_eq!(translate_edge_type(edge, ApiSchema::GitNexus), edge);
        }
    }

    #[test]
    fn schema_deserializes_from_query_string() {
        let def: ApiSchema = serde_json::from_str("\"default\"").unwrap();
        assert_eq!(def, ApiSchema::Default);
        let gn: ApiSchema = serde_json::from_str("\"gitnexus\"").unwrap();
        assert_eq!(gn, ApiSchema::GitNexus);
    }
}
