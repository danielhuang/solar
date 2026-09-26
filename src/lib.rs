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
/// Lowered tree intermediate representation.
pub mod tree_ir;
/// Tree IR interpreter.
pub mod tree_ir_interp;
/// Tree IR optimization passes.
pub mod tree_ir_opt;
/// Typed and monomorphized AST.
pub mod typed_ast;
/// Types shared by compiler stages.
pub mod types;
