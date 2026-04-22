//! Dart tree-sitter queries (ported from GitNexus `DART_QUERIES`).
//! Works with tree-sitter-dart (UserNobody14 grammar, ABI 14).
//!
//! Dart's grammar has function_signature/method_signature as wrappers;
//! top-level functions are `(program > function_signature)`, methods
//! inside classes are `(method_signature > function_signature)`. We
//! match top-level functions via `(program (function_signature ...))`
//! to avoid double-counting methods.

pub const QUERIES: &str = r#"
; ── Classes ──────────────────────────────────────────────────────────────────
(class_definition
  name: (identifier) @name) @definition.class

; ── Mixins ───────────────────────────────────────────────────────────────────
(mixin_declaration
  (identifier) @name) @definition.trait

; ── Extensions ───────────────────────────────────────────────────────────────
(extension_declaration
  name: (identifier) @name) @definition.class

; ── Enums ────────────────────────────────────────────────────────────────────
(enum_declaration
  name: (identifier) @name) @definition.enum

; ── Type aliases ─────────────────────────────────────────────────────────────
(type_alias
  (type_identifier) @name
  "=") @definition.type

; ── Top-level functions (parent is program, not method_signature) ────────────
(program
  (function_signature
    name: (identifier) @name) @definition.function)

; ── Abstract method declarations ─────────────────────────────────────────────
(declaration
  (function_signature
    name: (identifier) @name)) @definition.method

; ── Methods (inside class/mixin/extension bodies) ────────────────────────────
(method_signature
  (function_signature
    name: (identifier) @name)) @definition.method

; ── Constructors ─────────────────────────────────────────────────────────────
(constructor_signature
  name: (identifier) @name) @definition.constructor

; ── Factory constructors ─────────────────────────────────────────────────────
(method_signature
  (factory_constructor_signature
    (identifier) @name . (formal_parameter_list))) @definition.constructor

; ── Field declarations ───────────────────────────────────────────────────────
(declaration
  (type_identifier)
  (initialized_identifier_list
    (initialized_identifier
      (identifier) @name))) @definition.property

; ── Nullable field declarations (String? name) ──────────────────────────────
(declaration
  (nullable_type)
  (initialized_identifier_list
    (initialized_identifier
      (identifier) @name))) @definition.property

; ── Getters ──────────────────────────────────────────────────────────────────
(method_signature
  (getter_signature
    name: (identifier) @name)) @definition.property

; ── Setters ──────────────────────────────────────────────────────────────────
(method_signature
  (setter_signature
    name: (identifier) @name)) @definition.property

; ── Imports ──────────────────────────────────────────────────────────────────
(import_or_export
  (library_import
    (import_specification
      (configurable_uri) @import.source))) @import

; ── Calls: direct function/constructor calls ────────────────────────────────
(expression_statement
  (identifier) @call.name
  .
  (selector (argument_part))) @call

; ── Calls: method calls (obj.method()) ───────────────────────────────────────
(expression_statement
  (selector
    (unconditional_assignable_selector
      (identifier) @call.name))) @call

; ── Calls: in return statements (return User()) ─────────────────────────────
(return_statement
  (identifier) @call.name
  (selector (argument_part))) @call

; ── Calls: in variable assignments (var x = getUser()) ──────────────────────
(initialized_variable_definition
  value: (identifier) @call.name
  (selector (argument_part))) @call

; ── Re-exports (export 'foo.dart') ───────────────────────────────────────────
(import_or_export
  (library_export
    (configurable_uri) @import.source)) @import

; ── Write access: obj.field = value ──────────────────────────────────────────
(assignment_expression
  left: (assignable_expression
    (identifier) @assignment.receiver
    (unconditional_assignable_selector
      (identifier) @assignment.property))
  right: (_)) @assignment

; ── Write access: this.field = value ─────────────────────────────────────────
(assignment_expression
  left: (assignable_expression
    (this) @assignment.receiver
    (unconditional_assignable_selector
      (identifier) @assignment.property))
  right: (_)) @assignment

; ── Heritage: extends ────────────────────────────────────────────────────────
(class_definition
  name: (identifier) @heritage.class
  superclass: (superclass
    (type_identifier) @heritage.extends)) @heritage

; ── Heritage: implements ─────────────────────────────────────────────────────
(class_definition
  name: (identifier) @heritage.class
  interfaces: (interfaces
    (type_identifier) @heritage.implements)) @heritage.impl

; ── Heritage: with (mixins) ──────────────────────────────────────────────────
(class_definition
  name: (identifier) @heritage.class
  superclass: (superclass
    (mixins
      (type_identifier) @heritage.trait))) @heritage
"#;

pub const QUERY_SET: super::QuerySet = super::bundle(QUERIES);
