//! Solar compiler and interpreter.

/// Source-level abstract syntax tree.
pub mod ast;
/// AST interpreter.
pub mod ast_interp;
/// C code generation.
pub mod codegen;
/// Untyped AST with surface syntax normalized.
pub mod desugared_ast;
/// Compiler diagnostics and source mapping.
pub mod error;
/// Solar source formatter.
pub mod fmt;
/// Interpreter file and directory support.
pub mod interp_io;
/// Compiler intrinsics.
pub mod intrinsics;
/// Lowered intermediate representation.
pub mod ir;
/// IR interpreter.
pub mod ir_interp;
/// IR optimization passes.
pub mod ir_opt;
/// AST with final symbol names.
pub mod mangled_ast;
/// Solar parser.
pub mod parser;
/// Compiler pipeline stages.
pub mod pipeline;
/// Module and import resolution.
pub mod resolve;
/// Name-resolved AST and compiler-supplied definitions.
pub mod resolved_ast;
/// Lexical scope utilities.
pub mod scope;
/// Typed and monomorphized AST.
pub mod typed_ast;
/// Types shared by compiler stages.
pub mod types;
