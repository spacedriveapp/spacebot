//! Ruby tree-sitter queries (ported from GitNexus `RUBY_QUERIES`).
//! Works with tree-sitter-ruby.
//!
//! Note: Ruby `call` captures everything — require, include, extend,
//! attr_accessor, plus regular method calls. Post-processing routes:
//! - require/require_relative → import extraction
//! - include/extend/prepend → heritage (mixin) extraction
//! - attr_accessor/attr_reader/attr_writer → property definition
//! - everything else → regular call

pub const QUERIES: &str = r#"
; ── Modules ──────────────────────────────────────────────────────────────────
(module
  name: (constant) @name) @definition.module

; ── Classes ──────────────────────────────────────────────────────────────────
(class
  name: (constant) @name) @definition.class

; ── Instance methods ─────────────────────────────────────────────────────────
(method
  name: (identifier) @name) @definition.method

; ── Singleton (class-level) methods ──────────────────────────────────────────
(singleton_method
  name: (identifier) @name) @definition.method

; ── All calls (require, include, attr_*, and regular calls routed downstream) ─
(call
  method: (identifier) @call.name) @call

; ── Bare calls without parens ─────────────────────────────────────────────────
(body_statement
  (identifier) @call.name @call)

; ── Heritage: class < SuperClass ─────────────────────────────────────────────
(class
  name: (constant) @heritage.class
  superclass: (superclass
    (constant) @heritage.extends)) @heritage

; Write access: obj.field = value (Ruby setter — syntactically a method call to field=)
(assignment
  left: (call
    receiver: (_) @assignment.receiver
    method: (identifier) @assignment.property)
  right: (_)) @assignment

; Write access: obj.field += value (operator_assignment node)
(operator_assignment
  left: (call
    receiver: (_) @assignment.receiver
    method: (identifier) @assignment.property)
  right: (_)) @assignment
"#;

pub const QUERY_SET: super::QuerySet = super::bundle(QUERIES);
