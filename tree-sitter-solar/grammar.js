/// <reference types="tree-sitter-cli/dsl" />

module.exports = grammar({
  name: "solar",

  extras: ($) => [/\s/, $.comment],

  word: ($) => $.identifier,

  conflicts: ($) => [
    [$.if_statement, $.if_expression],
    [$.fn_type_param, $.named_type],
    [$.closure_param_list],
    [$.function_type],
    [$.named_type],
    [$.parenthesized_expression, $.tuple_literal],
    [$.struct_pattern_field],
    [$.named_type, $.qualified_type],
    [$.struct_literal, $.path_segment],
    // The bitwise `&`/`^` binary operators share their token with the postfix
    // reference/unique operators; GLR resolves by which parse completes. The
    // unary `!` adds a third interpretation (`!a ^ b` vs `!(a^)`), so its
    // conflicts list all three rules.
    [$.binary_expression, $.reference_expr],
    [$.binary_expression, $.unique_expr],
    [$.binary_expression, $.not_expression, $.reference_expr],
    [$.binary_expression, $.not_expression, $.unique_expr],
  ],

  rules: {
    source_file: ($) => repeat($._top_level_item),

    _top_level_item: ($) => choice($.struct_def, $.function_def, $.enum_def, $.method_def, $.import_statement, $.type_alias_def, $.const_def, $.static_def),

    // ── Const ──────────────────────────────────────────────
    const_def: ($) =>
      seq(
        optional("pub"),
        "const",
        field("name", $.identifier),
        optional(seq(":", field("type", $._type))),
        "=",
        field("value", $._expression),
        ";",
      ),

    // ── Static (global mutable state; top-level only) ─────
    static_def: ($) =>
      seq(
        optional("pub"),
        "static",
        field("name", $.identifier),
        optional(seq(":", field("type", $._type))),
        "=",
        field("value", $._expression),
        ";",
      ),

    comment: (_) => token(seq("//", /.*/)),

    // ── Import ─────────────────────────────────────────────
    import_statement: ($) => seq(
      optional("pub"),
      "import",
      choice(
        seq("{", $.import_name_list, "}"),
        field("module_name", $.identifier),
        "*",
      ),
      "from",
      $.string_literal,
      ";",
    ),

    import_name_list: ($) => seq($.import_path, repeat(seq(",", $.import_path)), optional(",")),

    import_path: ($) => seq($.identifier, repeat(seq("::", $.identifier))),

    // ── Type alias ─────────────────────────────────────────
    type_alias_def: ($) => seq(
      optional("pub"), "type",
      field("name", $.identifier),
      optional(field("type_params", $.type_params)),
      "=", field("target_type", $._type), ";"
    ),

    // ── Struct ──────────────────────────────────────────────
    struct_def: ($) =>
      seq(optional("pub"), "struct", field("name", $.identifier), optional(field("type_params", $.type_params)), "{", optional($.field_list), "}"),

    field_list: ($) => repeat1(seq($.field_def, ",")),

    field_def: ($) =>
      seq(optional("pub"), field("name", $.identifier), ":", field("type", $._type)),

    // ── Enum ───────────────────────────────────────────────
    enum_def: ($) =>
      seq(optional("pub"), "enum", field("name", $.identifier), optional(field("type_params", $.type_params)), "{", optional($.variant_list), "}"),

    variant_list: ($) => repeat1(seq($.variant_def, ",")),

    variant_def: ($) =>
      seq(field("name", $.identifier), optional(seq("(", field("inner_type", $._type), ")"))),

    // ── Function ────────────────────────────────────────────
    // Optional inline hint: `fn(inline) name(...)` / `method(inline) name(...)`.
    inline_attr: ($) => seq("(", "inline", ")"),

    function_def: ($) =>
      seq(
        optional("pub"),
        "fn",
        optional(field("attr", $.inline_attr)),
        field("name", $.identifier),
        optional(field("type_params", $.type_params)),
        "(",
        optional($.parameter_list),
        ")",
        optional(seq("->", field("return_type", $._type))),
        field("body", $.block),
      ),

    method_def: ($) =>
      seq(
        optional("pub"),
        "method",
        optional(field("attr", $.inline_attr)),
        field("name", $.identifier),
        optional(field("type_params", $.type_params)),
        "(",
        optional($.parameter_list),
        ")",
        optional(seq("->", field("return_type", $._type))),
        field("body", $.block),
      ),

    parameter_list: ($) => seq($.parameter, repeat(seq(",", $.parameter)), optional(",")),

    parameter: ($) =>
      choice(
        // normal param, or explicitly-typed keyword param with a default
        seq(
          field("pattern", $._destructure_target),
          ":",
          field("type", $._type),
          optional(seq("=", field("default", $._expression))),
        ),
        // keyword param with the type inferred from its default
        seq(field("pattern", $._destructure_target), "=", field("default", $._expression)),
      ),

    block: ($) => seq("{", repeat($._statement), optional(field("tail", $._expression_with_struct)), "}"),

    // ── Statements ──────────────────────────────────────────
    _statement: ($) =>
      choice($.let_statement, $.assignment_statement, $.expression_statement, $.if_statement, $.while_statement, $.for_statement, $.reflect_fields_statement, $.reflect_fields_pair_statement, $.reflect_variant_statement, $.reflect_variant_pair_statement, $.return_statement, $.break_statement, $.continue_statement, $.try_statement, $.function_def, $.const_def),

    return_statement: ($) =>
      seq("return", field("value", $._expression_with_struct), ";"),

    // The `;` is optional so `loop { break 5 }` works as a tail-less block. The
    // value excludes bare blocks/struct-literals to avoid `break {` ambiguity.
    // `prec.right` makes the parser prefer attaching a following expression as
    // the value rather than treating the `break` as valueless.
    break_statement: ($) => prec.right(seq("break", optional(field("value", $._expression)), optional(";"))),

    continue_statement: (_) => seq("continue", optional(";")),

    if_statement: ($) =>
      seq("if", field("condition", $._expression), field("body", $.block),
          optional(seq("else", field("else_body", choice($.if_statement, $.block))))),

    // `prec.dynamic(1, …)` breaks the GLR tie against `if_statement` for an
    // `if … else …` sitting at the end of a block: both parses are viable
    // there, and without an explicit preference the winner was arbitrary — it
    // flipped to `if_statement` when the enclosing item was the last thing in
    // the file, silently demoting a multiline tail `if/else` from the block's
    // value to a statement ("body does not end with an expression"). In true
    // statement position (more statements follow) only `if_statement` is
    // viable, so this preference changes nothing there.
    if_expression: ($) =>
      prec.dynamic(1,
        seq("if", field("condition", $._expression), field("then_body", $.block),
            "else", field("else_body", choice($.if_expression, $.block)))),

    while_statement: ($) =>
      seq("while", field("condition", $._expression), field("body", $.block)),

    // `try { … } catch (e) { … }` — desugars in the parser to a call of the
    // `try` intrinsic with two closures (body `\ { … }`, handler `\ e { … }`).
    // The binding `e` is a `&[Uint8]` (the thrown message); its type annotation
    // is optional and, if present, must be `&[Uint8]`.
    try_statement: ($) =>
      seq("try", field("body", $.block),
          "catch", "(",
          field("binding", $.identifier),
          optional(seq(":", field("binding_type", $._type))),
          ")",
          field("handler", $.block)),

    // `loop` is an expression (it can yield a value via `break <v>`), but it may
    // also stand alone as a statement without a trailing semicolon, like `while`.
    loop_expression: ($) => seq("loop", field("body", $.block)),

    for_statement: ($) => choice(
      seq("for", field("variable", $.identifier), "in",
          field("start", $._expression), "..", field("end", $._expression),
          field("body", $.block)),
      seq("for", field("variable", $.identifier), "in",
          field("iterable", $._expression), field("body", $.block)),
    ),

    let_statement: ($) =>
      seq(
        "let",
        field("pattern", $._destructure_target),
        optional(seq(":", field("type", $._type))),
        "=",
        field("value", $._expression_with_struct),
        ";",
      ),

    assignment_statement: ($) =>
      seq(field("target", $._expression), "=", field("value", $._expression_with_struct), ";"),

    expression_statement: ($) => seq($._expression, ";"),

    // ── Expressions ─────────────────────────────────────────
    // struct_literal is excluded here to avoid ambiguity with if/while blocks.
    // Use _expression_with_struct in value positions (after =, in arguments, etc.)
    _expression: ($) =>
      choice(
        $.identifier,
        $.float_literal,
        $.integer_literal,
        $.boolean_literal,
        $.null_expr,
        $.string_literal,
        $.char_literal,
        $.array_literal,
        $.array_repeat,
        $.binary_expression,
        $.not_expression,
        $.if_expression,
        $.loop_expression,
        $.closure_expr,
        $.path_expr,
        $.match_expression,
        $.reflect_match_expression,
        $._postfix_expression,
      ),

    _expression_with_struct: ($) =>
      choice(
        $._expression,
        $.struct_literal,
        $.block,
      ),

    _postfix_expression: ($) =>
      choice($.field_access, $.deref_expr, $.reference_expr, $.unique_expr, $.call_expr, $.generic_call_expr, $.generic_method_call, $.index_expr, $.slice_expr, $.parenthesized_expression, $.tuple_literal),

    parenthesized_expression: ($) => seq("(", $._expression_with_struct, ")"),

    tuple_literal: ($) =>
      seq("(", $._expression_with_struct, ",", $._expression_with_struct, repeat(seq(",", $._expression_with_struct)), optional(","), ")"),

    // Precedence follows Rust (loosest → tightest). All binary operators must
    // stay below the postfix operators (deref/reference/unique at 70) so that
    // `a@`, `a&`, `a^` bind tighter than any binary op. The bitwise `&` and `^`
    // tokens are also postfix operators (reference/unique); the GLR conflicts
    // declared above let tree-sitter pick the binary parse when a right operand
    // follows and the postfix parse otherwise.
    binary_expression: ($) => {
      const table = [
        [10, choice('||')],
        [15, choice('&&')],
        [20, choice('==', '!=')],
        [25, choice('<', '<=', '>', '>=')],
        [30, choice('|')],
        [35, choice('^')],
        [40, choice('&')],
        [45, choice('<<', '>>')],
        // Doubled operators are wrapping (overflow-never-panics) arithmetic, at
        // the same precedence as their checked single-character counterparts.
        [50, choice('+', '-', '++', '--')],
        [55, choice('*', '/', '%', '**')],
      ];
      return choice(...table.map(([p, op]) =>
        prec.left(p, seq(
          field('left', $._expression),
          field('operator', op),
          field('right', $._expression),
        ))
      ));
    },

    // Unary `!`: logical not (Bool) or bitwise complement (integers). Binds
    // looser than the postfix operators (so `!a.b` is `!(a.b)`, `!a@` is
    // `!(a@)`) but tighter than every binary operator (so `!a & b` is
    // `(!a) & b`), matching Rust.
    not_expression: ($) =>
      prec.right(65, seq('!', field('operand', $._expression))),

    field_access: ($) =>
      prec.left(80, seq(field("object", $._expression), ".", field("field", choice($.identifier, $.integer_literal)))),

    deref_expr: ($) => prec.left(70, seq(field("operand", $._expression), "@")),

    call_expr: ($) =>
      prec.left(90, seq(field("function", $._expression), "(", optional($.argument_list), ")")),

    generic_call_expr: ($) =>
      prec.left(90, seq(
        field("function", $.identifier),
        field("type_args", $.type_args),
        "(", optional($.argument_list), ")"
      )),

    generic_method_call: ($) =>
      prec.left(91, seq(
        field("receiver", $._expression),
        ".",
        field("method", $.identifier),
        field("type_args", $.type_args),
        "(", optional($.argument_list), ")"
      )),

    argument_list: ($) => seq($.argument, repeat(seq(",", $.argument)), optional(",")),

    argument: ($) =>
      choice(
        field("value", $._expression_with_struct),
        // keyword argument: name = value
        seq(field("name", $.identifier), "=", field("value", $._expression_with_struct)),
      ),

    index_expr: ($) =>
      prec.left(90, seq(field("object", $._expression), "[", field("index", $._expression), "]")),

    slice_expr: ($) =>
      prec.left(90, seq(
        field("object", $._expression),
        "[",
        field("start", $._expression),
        "..",
        field("end", $._expression),
        "]"
      )),

    reference_expr: ($) => prec.left(70, seq(field("operand", $._expression), "&")),

    unique_expr: ($) => prec.left(70, seq(field("operand", $._expression), "^")),

    struct_literal: ($) =>
      seq(
        optional(seq(field("module", $.identifier), "::")),
        field("name", $.identifier),
        optional(field("type_args", $.type_args)),
        "{",
        optional($.field_init_list),
        "}",
      ),

    field_init_list: ($) => seq($.field_init, repeat(seq(",", $.field_init)), optional(",")),

    field_init: ($) =>
      seq(field("name", $.identifier), ":", field("value", $._expression_with_struct)),

    // Optional element-type annotation `[…]#[T]` — required for `[]` (nothing
    // to infer from), checked against the inferred element type otherwise.
    array_literal: ($) =>
      seq("[", optional($.element_list), "]", optional(field("type_args", $.type_args))),

    array_repeat: ($) =>
      seq("[", field("element", $._expression_with_struct), ";", field("count", $._expression), "]"),

    element_list: ($) => seq($._expression_with_struct, repeat(seq(",", $._expression_with_struct)), optional(",")),

    // ── Closures ──────────────────────────────────────────────
    closure_expr: ($) => prec.right(1, seq(
      "\\",
      optional($.closure_param_list),
      optional(seq("->", field("return_type", $._type))),
      field("body", choice($._expression, $.block)),
    )),

    closure_param_list: ($) => seq($.closure_param, repeat(seq(",", $.closure_param))),

    closure_param: ($) => prec(2, seq(field("name", $.identifier), optional(seq(":", field("type", $._type))))),

    // ── Path expressions ─────────────────────────────────────
    // Arbitrary-length :: paths: A::B, A::B::C, mod::Enum::Variant, mod1::mod2::Enum::Variant, etc.
    // The parser produces a flat list of segments; resolve disambiguates.
    path_expr: ($) =>
      prec(1, seq($.path_segment, repeat1(seq("::", $.path_segment)))),

    path_segment: ($) =>
      seq(field("name", $.identifier), optional(field("type_args", $.type_args))),

    // ── Match expressions ──────────────────────────────────
    match_expression: ($) =>
      seq("match", field("scrutinee", $._expression), "{",
        optional($.match_arm_list), "}"),

    match_arm_list: ($) => seq($.match_arm, repeat(seq(",", $.match_arm)), optional(",")),

    match_arm: ($) =>
      seq(field("pattern", $.match_pattern), "=>", field("body", $._expression_with_struct)),

    match_pattern: ($) => choice($.variant_pattern, $.unit_variant_pattern, $.wildcard_pattern),

    // ── Compile-time reflection ─────────────────────────────
    // match.reflect Type { "struct" => ..., "enum" => ..., _ => ... }
    reflect_match_expression: ($) =>
      seq("match", ".", "reflect", field("type", $._type), "{",
        optional($.reflect_match_arm_list), "}"),

    // for.reflect_fields x in o { ... } — unrolls over the fields of the
    // struct behind the reference `o`; x is ([Uint8]&, F&) per field
    // and may be destructured: for.reflect_fields (name, value) in o { ... }
    reflect_fields_statement: ($) =>
      seq("for", ".", "reflect_fields", field("variable", $._destructure_target), "in",
        field("object", $._expression), field("body", $.block)),

    // for.reflect_fields_pair (name, a, b) in (x, y) { ... } — reflects two
    // values of the SAME struct in lockstep; `object` is a 2-tuple `(x, y)` and
    // the body binds ([Uint8]&, F&, F&) per field (name + both field refs).
    reflect_fields_pair_statement: ($) =>
      seq("for", ".", "reflect_fields_pair", field("variable", $._destructure_target), "in",
        field("object", $._expression), field("body", $.block)),

    // match.reflect_variant (variant, val) in o { ... } — desugars to a match
    // over the enum behind `o` with the body duplicated in every arm; binds
    // the ([Uint8]&, Payload) tuple of variant name and payload (a bare name
    // binds the whole tuple; unit variants bind only the name part)
    reflect_variant_statement: ($) =>
      seq("match", ".", "reflect_variant", field("pattern", $._destructure_target), "in",
        field("object", $._expression), field("body", $.block)),

    // match.reflect_variant_pair (name, idx, a, b) in (x, y) { ... } — reflects
    // two values of the SAME enum in lockstep; `object` is a 2-tuple `(x, y)`.
    // The body runs once per variant, only when both hold that variant.
    reflect_variant_pair_statement: ($) =>
      seq("match", ".", "reflect_variant_pair", field("pattern", $._destructure_target), "in",
        field("object", $._expression), field("body", $.block)),

    reflect_match_arm_list: ($) =>
      seq($.reflect_match_arm, repeat(seq(",", $.reflect_match_arm)), optional(",")),

    reflect_match_arm: ($) =>
      seq(field("pattern", choice($.string_literal, "_")), "=>", field("body", $._expression_with_struct)),

    // Enum::Variant(binding) or mod::...::Enum::Variant(binding)
    variant_pattern: ($) =>
      seq($.path_segment, repeat1(seq("::", $.path_segment)),
          "(", field("binding", $.identifier), ")"),

    // Enum::Variant or mod::...::Enum::Variant (no binding)
    unit_variant_pattern: ($) =>
      seq($.path_segment, repeat1(seq("::", $.path_segment))),

    wildcard_pattern: ($) => field("name", $.identifier),

    // ── Destructure patterns ────────────────────────────────
    _destructure_target: ($) => choice($.identifier, $.tuple_pattern, $.struct_pattern, $.array_pattern),

    tuple_pattern: ($) =>
      seq("(", $._destructure_target, ",", $._destructure_target,
          repeat(seq(",", $._destructure_target)), optional(","), ")"),

    struct_pattern: ($) =>
      seq(optional(seq(field("module", $.identifier), "::")),
          field("name", $.identifier), "{",
          $.struct_pattern_field, repeat(seq(",", $.struct_pattern_field)), optional(","),
          "}"),

    struct_pattern_field: ($) => choice(
      seq(field("field_name", $.identifier), ":", field("pattern", $._destructure_target)),
      field("field_name", $.identifier),
    ),

    array_pattern: ($) =>
      seq("[", $._destructure_target, repeat(seq(",", $._destructure_target)), optional(","), "]"),

    // ── Types ───────────────────────────────────────────────
    _type: ($) => choice($.named_type, $.qualified_type, $.reference_type, $.nullable_reference_type, $.unique_type, $.slice_type, $.fixed_array_type, $.function_type, $.tuple_type),

    named_type: ($) => seq($.identifier, optional(field("type_args", $.type_args))),

    // module-qualified type: mod::Type or mod::Type#[T]
    qualified_type: ($) => seq(field("module", $.identifier), "::", field("name", $.identifier), optional(field("type_args", $.type_args))),

    reference_type: ($) => seq("&", field("inner", $._type)),

    nullable_reference_type: ($) => seq("&", "?", field("inner", $._type)),

    unique_type: ($) => seq("^", field("inner", $._type)),

    slice_type: ($) => seq("[", field("element", $._type), "]"),

    fixed_array_type: ($) => seq("[", field("element", $._type), ";", field("size", $.integer_literal), "]"),

    function_type: ($) =>
      seq("fn", "(", optional($.fn_type_param_list), ")",
          optional(seq("->", field("return_type", $._type)))),

    tuple_type: ($) =>
      seq("(", $._type, ",", $._type, repeat(seq(",", $._type)), optional(","), ")"),

    fn_type_param_list: ($) =>
      seq($.fn_type_param, repeat(seq(",", $.fn_type_param)), optional(",")),

    fn_type_param: ($) =>
      seq(optional(seq(field("name", $.identifier), ":")), field("type", $._type)),

    // ── Generics ──────────────────────────────────────────────
    type_params: ($) => seq("#", "[", $.identifier, repeat(seq(",", $.identifier)), optional(","), "]"),

    type_args: ($) => seq("#", "[", $._type, repeat(seq(",", $._type)), optional(","), "]"),

    // ── Terminals ───────────────────────────────────────────
    identifier: (_) => /[a-zA-Z_][a-zA-Z0-9_]*/,

    integer_literal: (_) => /(0b[01]+|0o[0-7]+|0x[0-9a-fA-F]+|[0-9]+)(i8|i16|i32|i64|u8|u16|u32|u64|u)?/,
    // Floats require an `f`/`f32`/`f64` suffix, and a decimal point must be
    // followed by digits — so `1.f` stays a field access on the integer `1`
    // (the token can't match without digits after the dot) and `1.method()`
    // keeps parsing as a method call. `1f` and `1.0f` are floats.
    float_literal: (_) => /[0-9]+(\.[0-9]+)?(f32|f64|f)/,

    boolean_literal: (_) => choice("true", "false"),

    // null#[T] — the null value of a nullable reference type &?T
    null_expr: ($) => seq("null", field("type_args", $.type_args)),

    string_literal: (_) => /"([^"\\]|\\.)*"/,

    // A single-byte character literal, e.g. 'a', '\n', '\'', '\\'. Desugars to
    // a Uint8 integer literal in the parser. Exactly one char (or escape) long.
    char_literal: (_) => /'([^'\\]|\\.)'/,
  },
});
