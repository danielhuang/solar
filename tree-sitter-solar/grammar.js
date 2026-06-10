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
  ],

  rules: {
    source_file: ($) => repeat($._top_level_item),

    _top_level_item: ($) => choice($.struct_def, $.function_def, $.enum_def, $.method_def, $.import_statement, $.type_alias_def),

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
    function_def: ($) =>
      seq(
        optional("pub"),
        "fn",
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
      seq(field("pattern", $._destructure_target), ":", field("type", $._type)),

    block: ($) => seq("{", repeat($._statement), optional(field("tail", $._expression_with_struct)), "}"),

    // ── Statements ──────────────────────────────────────────
    _statement: ($) =>
      choice($.let_statement, $.assignment_statement, $.expression_statement, $.if_statement, $.while_statement, $.for_statement, $.return_statement, $.function_def),

    return_statement: ($) =>
      seq("return", field("value", $._expression_with_struct), ";"),

    if_statement: ($) =>
      seq("if", field("condition", $._expression), field("body", $.block),
          optional(seq("else", field("else_body", choice($.if_statement, $.block))))),

    if_expression: ($) =>
      seq("if", field("condition", $._expression), field("then_body", $.block),
          "else", field("else_body", choice($.if_expression, $.block))),

    while_statement: ($) =>
      seq("while", field("condition", $._expression), field("body", $.block)),

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
        $.integer_literal,
        $.boolean_literal,
        $.string_literal,
        $.array_literal,
        $.array_repeat,
        $.binary_expression,
        $.if_expression,
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

    binary_expression: ($) => {
      const table = [
        [10, choice('||')],
        [20, choice('&&')],
        [30, choice('==', '!=')],
        [40, choice('<', '<=', '>', '>=')],
        [50, choice('+', '-')],
        [60, choice('*', '/', '%')],
      ];
      return choice(...table.map(([p, op]) =>
        prec.left(p, seq(
          field('left', $._expression),
          field('operator', op),
          field('right', $._expression),
        ))
      ));
    },

    field_access: ($) =>
      prec.left(80, seq(field("object", $._expression), ".", field("field", choice($.identifier, $.integer_literal)))),

    deref_expr: ($) => prec.left(55, seq(field("operand", $._expression), "@")),

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

    argument_list: ($) => seq($._expression_with_struct, repeat(seq(",", $._expression_with_struct)), optional(",")),

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

    array_literal: ($) =>
      seq("[", optional($.element_list), "]"),

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
    _type: ($) => choice($.named_type, $.qualified_type, $.reference_type, $.unique_type, $.slice_type, $.fixed_array_type, $.function_type, $.tuple_type),

    named_type: ($) => seq($.identifier, optional(field("type_args", $.type_args))),

    // module-qualified type: mod::Type or mod::Type#[T]
    qualified_type: ($) => seq(field("module", $.identifier), "::", field("name", $.identifier), optional(field("type_args", $.type_args))),

    reference_type: ($) => seq("&", field("inner", $._type)),

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

    integer_literal: (_) => /[0-9]+(i8|i16|i32|i64|u8|u16|u32|u64|u)?/,

    boolean_literal: (_) => choice("true", "false"),

    string_literal: (_) => /"([^"\\]|\\.)*"/,
  },
});
