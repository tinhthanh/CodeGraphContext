/// Kotlin `enum class X`: an `enum` token somewhere under the modifiers.
fn has_enum_modifier(node: &Node) -> bool {
    fn contains_enum(n: &Node) -> bool {
        n.kind() == "enum" || (0..n.child_count()).filter_map(|i| n.child(i)).any(|c| contains_enum(&c))
    }
    (0..node.child_count())
        .filter_map(|i| node.child(i))
        .filter(|c| c.kind() == "modifiers")
        .any(|m| contains_enum(&m))
}

/// Map a type-declaration node to a language-neutral kind label
/// (the Interface/Struct/Object/... node labels upstream CGC uses).
pub fn class_kind(node: &Node) -> String {
    // Some extractors hand over the declaration's name node; use its parent.
    let parent;
    let node = if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "simple_identifier" | "name" | "constant" | "type_constructor"
    ) {
        match node.parent() {
            Some(p) => {
                parent = p;
                &parent
            }
            None => node,
        }
    } else {
        node
    };
    let token = |kw: &str| (0..node.child_count()).filter_map(|i| node.child(i)).any(|c| c.kind() == kw);
    let kind = match node.kind() {
        "interface_declaration" | "protocol_declaration" => "interface",
        "enum_declaration" | "enum_item" | "enum_specifier" | "enum_definition" | "enum" => "enum",
        "record_declaration" | "record_struct_declaration" => "record",
        "annotation_type_declaration" => "annotation",
        "struct_item" | "struct_specifier" | "struct_declaration" => "struct",
        "trait_item" | "trait_declaration" | "trait_definition" => "trait",
        "object_declaration" | "object_definition" => "object",
        "companion_object" => "companion",
        "union_item" | "union_specifier" => "union",
        "type_alias_declaration" | "type_alias" | "type_item" => "type_alias",
        "module" | "module_declaration" => "module",
        "mixin_declaration" => "mixin",
        "extension_declaration" => "extension",
        "preproc_def" | "preproc_function_def" | "macro_definition" => "macro",
        // Go: `type X struct {...}` / `type X interface {...}`
        "type_declaration" | "type_spec" => {
            let spec = if node.kind() == "type_spec" {
                Some(*node)
            } else {
                (0..node.named_child_count()).filter_map(|i| node.named_child(i)).find(|c| c.kind() == "type_spec")
            };
            match spec.and_then(|s| s.child_by_field_name("type")).map(|t| t.kind()) {
                Some("struct_type") => "struct",
                Some("interface_type") => "interface",
                _ => "type_alias",
            }
        }
        // Kotlin `interface X` / `enum class X`, Swift `struct`/`enum`/`extension`
        "class_declaration" if token("interface") => "interface",
        "class_declaration" if token("struct") => "struct",
        "class_declaration" if token("extension") => "extension",
        "class_declaration" if token("enum") || has_enum_modifier(node) => "enum",
        _ => "class",
    };
    kind.to_string()
}

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
pub mod kotlin;
pub mod scala;
pub mod swift;
pub mod haskell;
pub mod dart;
pub mod perl;
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
    /// Dependency-injection points. Only languages with a DI convention
    /// (Java/Spring) implement this.
    fn find_injections(&self, _root: &Node, _source: &[u8]) -> Vec<InjectionData> {
        Vec::new()
    }

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
            let mut name = parent
                .child_by_field_name("name")
                .map(|n| get_node_text(&n, source).to_string());

            // For arrow_function / function_expression without a name field,
            // look up to the parent variable_declarator or assignment for the name.
            // e.g., `const loadCvs = async () => { get() }` → name = "loadCvs"
            let is_anon_fn =
                parent.kind() == "arrow_function" || parent.kind() == "function_expression";
            if name.is_none() && is_anon_fn {
                if let Some(grandparent) = parent.parent() {
                    let name_node = match grandparent.kind() {
                        "variable_declarator" => grandparent.child_by_field_name("name"),
                        "assignment_expression" => {
                            // `Foo.prototype.bar = function(){}` → "bar"
                            grandparent.child_by_field_name("left").map(|left| {
                                if left.kind() == "member_expression" {
                                    left.child_by_field_name("property").unwrap_or(left)
                                } else {
                                    left
                                }
                            })
                        }
                        "pair" => grandparent.child_by_field_name("key"),
                        // Class field: `handler = () => {}`
                        "public_field_definition" | "field_definition" | "property_definition" => {
                            grandparent
                                .child_by_field_name("name")
                                .or_else(|| grandparent.child_by_field_name("property"))
                        }
                        _ => None,
                    };
                    name = name_node
                        .map(|n| get_node_text(&n, source).to_string())
                        .filter(|s| !s.is_empty());
                }
            }

            // An anonymous callback (e.g. `useEffect(() => { ... })`) is not a
            // useful context: keep walking up to the nearest named ancestor.
            if name.is_none() && is_anon_fn {
                curr = parent.parent();
                continue;
            }

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
        "kotlin" => Some(Box::new(kotlin::KotlinExtractor)),
        "scala" => Some(Box::new(scala::ScalaExtractor)),
        "swift" => Some(Box::new(swift::SwiftExtractor)),
        "haskell" => Some(Box::new(haskell::HaskellExtractor)),
        "dart" => Some(Box::new(dart::DartExtractor)),
        "perl" => Some(Box::new(perl::PerlExtractor)),
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
