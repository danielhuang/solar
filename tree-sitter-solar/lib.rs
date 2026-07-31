//! Tree-sitter language binding for Solar.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_solar() -> *const ();
}

/// The Solar tree-sitter language.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_solar) };
