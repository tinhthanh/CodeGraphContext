//! Vendored Perl grammar for tree-sitter, compatible with tree-sitter 0.25.
//!
//! This is a minimal wrapper around the tree-sitter-perl grammar C source,
//! avoiding the direct dependency on tree-sitter 0.26 that causes links conflicts.

use tree_sitter_language::LanguageFn;

extern "C" {
    fn tree_sitter_perl() -> *const ();
}

/// The tree-sitter [`LanguageFn`] for Perl.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_perl) };

#[cfg(test)]
mod tests {
    #[test]
    fn test_can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::LANGUAGE.into())
            .expect("Error loading Perl parser");
    }
}
