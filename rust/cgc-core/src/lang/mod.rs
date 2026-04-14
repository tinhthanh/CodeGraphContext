pub mod python;
pub mod javascript;
pub mod typescript;
pub mod tsx;
pub mod go;
pub mod java;
pub mod cpp;
pub mod c_lang;
pub mod rust_lang;
pub mod ruby;
pub mod csharp;
pub mod php;
// pub mod kotlin;  // Disabled: tree-sitter-kotlin requires tree-sitter 0.20
pub mod scala;
pub mod swift;
pub mod haskell;
pub mod dart;
// pub mod perl;  // Disabled: tree-sitter-perl requires tree-sitter 0.26
pub mod elixir;

use tree_sitter::{Language, Node};

use crate::types::*;

/// Trait that each language extractor must implement.
/// All methods receive the AST root node and source bytes.
pub trait LanguageExtractor: Send + Sync {
    /// The tree-sitter Language for this extractor.
    fn language(&self) -> Language;

    /// Language name string (e.g., "python", "go").
    fn lang_name(&self) -> &str;

    /// Extract function/method definitions from the AST.
    fn find_functions(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<FunctionData>;

    /// Extract class/struct/interface definitions from the AST.
    fn find_classes(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<ClassData>;

    /// Extract import statements from the AST.
    fn find_imports(&self, root: &Node, source: &[u8]) -> Vec<ImportData>;

    /// Extract function call sites from the AST.
    fn find_calls(&self, root: &Node, source: &[u8]) -> Vec<CallData>;

    /// Extract variable assignments from the AST.
    fn find_variables(&self, root: &Node, source: &[u8]) -> Vec<VariableData>;

    /// Pre-scan: extract top-level definition names for imports_map.
    fn pre_scan_definitions(&self, root: &Node, source: &[u8]) -> Vec<String> {
        let mut names = Vec::new();
        for f in self.find_functions(root, source, false) {
            if f.context.is_none() {
                names.push(f.name);
            }
        }
        for c in self.find_classes(root, source, false) {
            if c.context.is_none() {
                names.push(c.name);
            }
        }
        names
    }

    /// Node types that contribute to cyclomatic complexity.
    fn complexity_node_types(&self) -> &[&str];

    /// Calculate cyclomatic complexity by traversing the node.
    fn calculate_complexity(&self, node: &Node) -> usize {
        let types = self.complexity_node_types();
        let mut count = 1usize;
        walk_complexity(node, types, &mut count);
        count
    }
}

fn walk_complexity(node: &Node, types: &[&str], count: &mut usize) {
    if types.contains(&node.kind()) {
        *count += 1;
    }
    let child_count = node.child_count();
    for i in 0..child_count {
        if let Some(child) = node.child(i) {
            walk_complexity(&child, types, count);
        }
    }
}

// ---- Shared helpers ----

/// Extract text from a tree-sitter node.
pub fn get_node_text<'a>(node: &Node, source: &'a [u8]) -> &'a str {
    let start = node.start_byte();
    let end = node.end_byte();
    std::str::from_utf8(&source[start..end]).unwrap_or("")
}

/// Walk up the AST to find the enclosing context (function or class).
/// Returns (name, node_type, line_number).
pub fn get_parent_context(
    node: &Node,
    source: &[u8],
    types: &[&str],
) -> (Option<String>, Option<String>, Option<usize>) {
    let mut curr = node.parent();
    while let Some(parent) = curr {
        if types.contains(&parent.kind()) {
            let name = parent
                .child_by_field_name("name")
                .map(|n| get_node_text(&n, source).to_string());
            let kind = Some(parent.kind().to_string());
            let line = Some(parent.start_position().row + 1);
            return (name, kind, line);
        }
        curr = parent.parent();
    }
    (None, None, None)
}

/// Registry: get a language extractor by name.
pub fn get_extractor(lang_name: &str) -> Option<Box<dyn LanguageExtractor>> {
    match lang_name {
        "python" => Some(Box::new(python::PythonExtractor)),
        "javascript" => Some(Box::new(javascript::JavaScriptExtractor)),
        "typescript" => Some(Box::new(typescript::TypeScriptExtractor)),
        "tsx" => Some(Box::new(tsx::TsxExtractor)),
        "go" => Some(Box::new(go::GoExtractor)),
        "java" => Some(Box::new(java::JavaExtractor)),
        "cpp" => Some(Box::new(cpp::CppExtractor)),
        "c" => Some(Box::new(c_lang::CExtractor)),
        "rust" => Some(Box::new(rust_lang::RustExtractor)),
        "ruby" => Some(Box::new(ruby::RubyExtractor)),
        "c_sharp" => Some(Box::new(csharp::CSharpExtractor)),
        "php" => Some(Box::new(php::PhpExtractor)),
        // "kotlin" disabled: tree-sitter-kotlin requires tree-sitter 0.20
        "scala" => Some(Box::new(scala::ScalaExtractor)),
        "swift" => Some(Box::new(swift::SwiftExtractor)),
        "haskell" => Some(Box::new(haskell::HaskellExtractor)),
        "dart" => Some(Box::new(dart::DartExtractor)),
        // "perl" disabled: tree-sitter-perl requires tree-sitter 0.26
        "elixir" => Some(Box::new(elixir::ElixirExtractor)),
        _ => None,
    }
}

/// Registry: get a language extractor by file extension.
pub fn get_extractor_by_ext(ext: &str) -> Option<Box<dyn LanguageExtractor>> {
    let lang = match ext {
        ".py" | ".ipynb" => "python",
        ".js" | ".jsx" | ".mjs" | ".cjs" => "javascript",
        ".ts" => "typescript",
        ".tsx" => "tsx",
        ".go" => "go",
        ".java" => "java",
        ".cpp" | ".cc" | ".cxx" | ".hpp" | ".hh" => "cpp",
        ".c" | ".h" => "c",
        ".rs" => "rust",
        ".rb" => "ruby",
        ".cs" => "c_sharp",
        ".php" => "php",
        ".kt" => "kotlin",
        ".scala" | ".sc" => "scala",
        ".swift" => "swift",
        ".hs" => "haskell",
        ".dart" => "dart",
        ".pl" | ".pm" => "perl",
        ".ex" | ".exs" => "elixir",
        _ => return None,
    };
    get_extractor(lang)
}
