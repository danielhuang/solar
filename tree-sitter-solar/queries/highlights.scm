; Syntax highlighting for the Solar language.
;
; Convention: first-match-wins (tree-sitter `highlight` crate / Helix). Specific
; patterns are listed before the general `(identifier) @variable` fallback so the
; more precise capture takes precedence.

; ── Comments ───────────────────────────────────────────────────────────────
(comment) @comment

; ── Literals ───────────────────────────────────────────────────────────────
(integer_literal) @number
(float_literal) @number.float
(boolean_literal) @boolean
(string_literal) @string

; `null#[T]`
(null_expr "null" @constant.builtin)

; ── Definitions ────────────────────────────────────────────────────────────
(function_def name: (identifier) @function)
(method_def name: (identifier) @function.method)

(struct_def name: (identifier) @type)
(enum_def name: (identifier) @type)
(type_alias_def name: (identifier) @type)

(field_def name: (identifier) @variable.member)
(variant_def name: (identifier) @constructor)

(const_def name: (identifier) @constant)
(static_def name: (identifier) @variable)

; Type parameter declarations: `#[T]`, `#[A, B]`
(type_params (identifier) @type.parameter)

; ── Parameters ─────────────────────────────────────────────────────────────
(parameter pattern: (identifier) @variable.parameter)
(closure_param name: (identifier) @variable.parameter)
(fn_type_param name: (identifier) @variable.parameter)

; Keyword argument name at a call site: `f(step = 5)`
(argument name: (identifier) @variable.parameter)

; ── Calls ──────────────────────────────────────────────────────────────────
; plain call:  foo(...)
(call_expr
  function: (identifier) @function.call)

; method call:  x.foo(...)
(call_expr
  function: (field_access
    field: (identifier) @function.method.call))

; generic call:  foo#[T](...)
(generic_call_expr
  function: (identifier) @function.call)

; generic method call:  x.foo#[T](...)
(generic_method_call
  method: (identifier) @function.method.call)

; ── Field access & struct/enum construction ────────────────────────────────
(field_access
  field: (identifier) @variable.member)

(field_init name: (identifier) @variable.member)
(struct_pattern_field field_name: (identifier) @variable.member)

(struct_literal name: (identifier) @constructor)
(struct_literal module: (identifier) @module)
(struct_pattern name: (identifier) @constructor)
(struct_pattern module: (identifier) @module)

; ── Imports ────────────────────────────────────────────────────────────────
(import_statement module_name: (identifier) @module)
(import_path (identifier) @module)

; ── Paths & patterns ───────────────────────────────────────────────────────
; In an `A::B::C` path the final segment is the referenced item and the earlier
; ones are modules. We can't resolve the item's kind structurally, so fall back
; to a naming heuristic on the last segment (anchored with `.`):
;   * a call target             -> @function.call
;   * ALL_CAPS                  -> @constant
;   * Uppercase-initial         -> @constructor  (types / enum variants)
;   * otherwise                 -> @variable
; These must precede the catch-all module rule so the last segment wins.
(call_expr
  function: (path_expr (path_segment name: (identifier) @function.call) .))
((path_expr (path_segment name: (identifier) @constant) .)
  (#match? @constant "^[A-Z][A-Z0-9_]*$"))
((path_expr (path_segment name: (identifier) @constructor) .)
  (#match? @constructor "^[A-Z]"))
(path_expr (path_segment name: (identifier) @variable) .)
(path_expr (path_segment name: (identifier) @module))

; `Enum::Variant(x)` / `mod::…::Enum::Variant(x)`: the segment right before the
; `(` is the variant; earlier segments are the enum/module qualifiers.
(variant_pattern
  (path_segment name: (identifier) @constructor) . "(")
(variant_pattern
  (path_segment name: (identifier) @module))
(variant_pattern binding: (identifier) @variable)

; `Enum::Variant` / `mod::…::Enum::Variant` (no binding): last segment wins.
(unit_variant_pattern
  (path_segment name: (identifier) @constructor) .)
(unit_variant_pattern
  (path_segment name: (identifier) @module))

; ── Types ──────────────────────────────────────────────────────────────────
; Built-in primitive types.
((named_type (identifier) @type.builtin)
  (#any-of? @type.builtin
    "Int8" "Int16" "Int32" "Int64" "Int"
    "Uint8" "Uint16" "Uint32" "Uint64" "Uint"
    "Float32" "Float64" "Bool"))

(named_type (identifier) @type)
(qualified_type name: (identifier) @type)
(qualified_type module: (identifier) @module)

; ── Keywords ───────────────────────────────────────────────────────────────
[
  "struct"
  "enum"
  "type"
  "const"
  "static"
  "pub"
] @keyword

[
  "fn"
  "method"
] @keyword.function

"let" @keyword

[
  "import"
  "from"
] @keyword.import

[
  "if"
  "else"
  "match"
  "while"
  "for"
  "loop"
  "in"
] @keyword.conditional

[
  "return"
  "break"
  "continue"
] @keyword.return

[
  "try"
  "catch"
] @keyword.exception

; Compile-time reflection keywords.
[
  "reflect"
  "reflect_fields"
  "reflect_fields_pair"
  "reflect_variant"
  "reflect_variant_pair"
] @keyword

(inline_attr "inline" @attribute)

; ── Operators & punctuation ────────────────────────────────────────────────
(binary_expression operator: _ @operator)
(not_expression "!" @operator)

[
  "="
  "->"
  "=>"
  ".."
  "@"
  "&"
  "^"
  "?"
  "\\"
] @operator

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

; Generic delimiter `#[ … ]`
(type_args "#" @punctuation.special)
(type_params "#" @punctuation.special)

[
  ","
  ":"
  ";"
  "::"
  "."
] @punctuation.delimiter

; ── Fallback ───────────────────────────────────────────────────────────────
(identifier) @variable
