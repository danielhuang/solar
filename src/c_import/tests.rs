use super::*;

fn request(names: &[&str]) -> ImportKind {
    ImportKind::Named(
        names
            .iter()
            .map(|name| ImportName {
                segments: vec![(*name).into()],
            })
            .collect(),
    )
}

fn header(source: &str, names: &[&str]) -> Result<Vec<TopLevelItem>, String> {
    let directory = tempdir::TempDir::new("solar-header-parser").unwrap();
    let path = directory.path().join("test.h");
    std::fs::write(&path, source).unwrap();
    import_uncached(path.to_str().unwrap(), &request(names)).map(|result| result.items)
}

fn integer(items: &[TopLevelItem], name: &str) -> (i128, String) {
    let constant = items
        .iter()
        .find_map(|item| match item {
            TopLevelItem::Const(c) if c.name == name => Some(c),
            _ => None,
        })
        .unwrap();
    let ExprKind::IntegerLiteral(value, _) = constant.value.kind else {
        panic!("{name} is not an integer");
    };
    let Some(Type::Named(ty)) = &constant.ty else {
        panic!("{name} has no scalar type");
    };
    (value, ty.name.to_string())
}

#[test]
fn preprocessing_expands_macros_and_selects_only_active_branches() {
    let items = header(
        r#"
        #define JOIN_IMPL(a, b) a ## b
        #define JOIN(a, b) JOIN_IMPL(a, b)
        #define SUFFIX 7
        #define VALUE7 23
        #define ID(x) x
        #define ALIAS ID
        #define SELF SELF
        enum { SELF = 9 };
        #define ACTIVE (ALIAS(JOIN(VALUE, SUFFIX)) + SELF)
        #if defined(SUFFIX) && !defined(MISSING) && (0 && 1 / 0 || 1)
        #define PICKED ACTIVE
        #elif 1 / 0
        #error inactive elif must not be evaluated
        #else
        #include "does-not-exist.h"
        #endif
        #if 0
        #if unsupported(inactive syntax)
        #error inactive nesting
        #endif
        #endif
        #define CONTINUED (1 + \
            2 /* a comment
                 spanning lines */ + 3)
        #define FUNCTION(x) ((x) + 1)
        #define REPEATED (FUNCTION(FUNCTION(1)) + FUNCTION(1))
    "#,
        &["PICKED", "CONTINUED", "REPEATED"],
    )
    .unwrap();
    assert_eq!(integer(&items, "PICKED"), (32, "Int32".into()));
    assert_eq!(integer(&items, "CONTINUED"), (6, "Int32".into()));
    assert_eq!(integer(&items, "REPEATED"), (5, "Int32".into()));
}

#[test]
fn macro_stringification_preserves_token_spacing() {
    let items = header(
        r#"
        #define STR(x) #x
        #define EXPAND_STR(x) STR(x)
        #define EXPR 1+2
        #define TEXT STR(a+b) "|" STR(a  + /**/ b) "|" EXPAND_STR(EXPR)
    "#,
        &["TEXT"],
    )
    .unwrap();
    let TopLevelItem::Const(value) = items.last().unwrap() else {
        panic!("missing string");
    };
    let ExprKind::ArrayLiteral(elements, _) = &value.value.kind else {
        panic!("not a C string");
    };
    let bytes: Vec<_> = elements
        .iter()
        .map(|e| match e.kind {
            ExprKind::IntegerLiteral(n, _) => n as u8,
            _ => panic!("not a character"),
        })
        .collect();
    assert_eq!(bytes, b"a+b|a + b|1+2\0");
}

#[test]
fn constant_arithmetic_preserves_c_promotions_widths_and_signedness() {
    let items = header(
        r#"
        #define OCTAL 0777
        #define HEX 0xffffffff
        #define ALL32 (~0U)
        #define ALL64 (~0ULL)
        #define WRAPPED ((~0ULL) * (~0ULL))
        #define PROMOTED ((unsigned char)255 + (short)-2)
        #define MIXED (-1L < 1U)
        #define UNSIGNED (-1LL < 1UL)
        #define CONDITIONAL (1 ? -1 : 1U)
        #define LAZY (0 ? (1 / 0) : 27)
        #define BOOLCAST ((_Bool)256)
        enum { FIRST = 17, SECOND, THIRD = (1U << 31) };
    "#,
        &[
            "OCTAL",
            "HEX",
            "ALL32",
            "ALL64",
            "WRAPPED",
            "PROMOTED",
            "MIXED",
            "UNSIGNED",
            "CONDITIONAL",
            "LAZY",
            "BOOLCAST",
            "SECOND",
            "THIRD",
        ],
    )
    .unwrap();
    for (name, value, ty) in [
        ("OCTAL", 511, "Int32"),
        ("HEX", 4294967295, "Uint32"),
        ("ALL32", 4294967295, "Uint32"),
        ("ALL64", 18446744073709551615, "Uint64"),
        ("WRAPPED", 1, "Uint64"),
        ("PROMOTED", 253, "Int32"),
        ("MIXED", 1, "Int32"),
        ("UNSIGNED", 0, "Int32"),
        ("CONDITIONAL", 4294967295, "Uint32"),
        ("LAZY", 27, "Int32"),
        ("SECOND", 18, "Uint32"),
        ("THIRD", 2147483648, "Uint32"),
    ] {
        assert_eq!(integer(&items, name), (value, ty.into()), "{name}");
    }
    assert!(items.iter().any(|item| matches!(item, TopLevelItem::Const(c) if c.name == "BOOLCAST" && matches!(c.value.kind, ExprKind::BooleanLiteral(true)))));
}

#[test]
fn enums_choose_a_type_for_the_whole_declaration() {
    let items = header(
        r#"
        enum Small { SMALL = 1UL };
        enum Large { LOW = 1, HIGH = 0xffffffffffffffffULL };
        enum Signed { NEGATIVE = -1, POSITIVE = 0x80000000U };
        enum Increment { LAST32 = 0xffffffffU, FIRST64 };
        #if (1 == 1) << 40
        #define PREPROCESSOR_WIDTH 1
        #else
        #error preprocessor arithmetic must use intmax_t
        #endif
    "#,
        &[
            "SMALL",
            "LOW",
            "HIGH",
            "NEGATIVE",
            "POSITIVE",
            "LAST32",
            "FIRST64",
            "PREPROCESSOR_WIDTH",
        ],
    )
    .unwrap();
    for (name, value, ty) in [
        ("SMALL", 1, "Int32"),
        ("LOW", 1, "Uint"),
        ("HIGH", 18446744073709551615, "Uint"),
        ("NEGATIVE", -1, "Int"),
        ("POSITIVE", 2147483648, "Int"),
        ("LAST32", 4294967295, "Uint"),
        ("FIRST64", 4294967296, "Uint"),
        ("PREPROCESSOR_WIDTH", 1, "Int32"),
    ] {
        assert_eq!(integer(&items, name), (value, ty.into()), "{name}");
    }
}

#[test]
fn structs_keep_layout_and_declarator_binding() {
    let items = header(
        r#"
        typedef unsigned long Count;
        struct Inner { short a, b; };
        typedef struct Outer {
            char tag;
            struct Inner values[2];
            int (*matrix)[3];
            const struct Outer *next;
            unsigned char padding[16 - sizeof(struct Inner) - sizeof(Count)];
        } Outer;
        #define SIZE sizeof(Outer)
        #define ALIGN _Alignof(Outer)
    "#,
        &["Outer", "SIZE", "ALIGN"],
    )
    .unwrap();
    assert_eq!(integer(&items, "SIZE"), (40, "Uint".into()));
    assert_eq!(integer(&items, "ALIGN"), (8, "Uint".into()));
    let fields = items
        .iter()
        .find_map(|item| match item {
            TopLevelItem::Struct(s) if s.fields.iter().any(|f| f.name == "matrix") => {
                Some(&s.fields)
            }
            _ => None,
        })
        .unwrap();
    assert!(
        matches!(&fields[2].ty, Type::NullableReference(inner) if matches!(inner.as_ref(), Type::FixedArray(_, 3)))
    );
    assert!(matches!(&fields[4].ty, Type::FixedArray(_, 4)));
}

#[test]
fn unsupported_or_invalid_requested_expressions_are_errors() {
    for (source, name, expected) in [
        ("#define BAD (4 / 0)", "BAD", "division by zero"),
        ("#define BAD (1U << 32)", "BAD", "shift count"),
        ("#define BAD (2147483647 + 1)", "BAD", "signed overflow"),
        ("#define BAD ((int *)1)", "BAD", "non-null"),
        ("#define BAD BAD", "BAD", "cannot evaluate"),
        ("#define BAD __builtin_unknown(1)", "BAD", "cannot evaluate"),
        (
            "#pragma pack(push, 1)\nstruct Bad { char x; int y; };",
            "Bad",
            "pragma pack",
        ),
        (
            "typedef struct { int x __attribute__((aligned(16))); } Bad;",
            "Bad",
            "attribute",
        ),
        (
            "struct Bad { int (*callback)(int); };",
            "Bad",
            "not representable",
        ),
        ("struct Bad { struct Bad recursive; };", "Bad", "recursive"),
    ] {
        let error = header(source, &[name]).unwrap_err();
        assert!(error.contains(expected), "{source}: {error}");
    }
}

#[test]
fn nested_includes_once_and_transitive_cache_invalidation() {
    let directory = tempdir::TempDir::new("solar-header-dependencies").unwrap();
    let child = directory.path().join("child.h");
    let root = directory.path().join("root.h");
    std::fs::write(
        &child,
        "#pragma once\n#define VALUE 3\nstruct Item { int x; };\n",
    )
    .unwrap();
    std::fs::write(&root, "#include \"child.h\"\n#include \"child.h\"\n").unwrap();
    let kind = request(&["VALUE", "Item"]);
    let initial = import_uncached(root.to_str().unwrap(), &kind).unwrap();
    assert_eq!(initial.dependencies.len(), 2);
    assert!(initial.dependencies.iter().any(|(p, _, _)| p == &child));
    assert_eq!(
        integer(&import(root.to_str().unwrap(), &kind).unwrap(), "VALUE").0,
        3
    );
    std::fs::write(
        &child,
        "#pragma once\n#define VALUE 12345\nstruct Item { int x; };\n",
    )
    .unwrap();
    assert_eq!(
        integer(&import(root.to_str().unwrap(), &kind).unwrap(), "VALUE").0,
        12345
    );
}
