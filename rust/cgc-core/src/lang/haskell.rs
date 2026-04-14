use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, get_parent_context, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    "if_then_else",
    "case_expression",
    "guard",
    "conditional",
];

/// Haskell top-level functions and bind expressions.
const QUERY_FUNCTIONS: &str = r#"
    (function) @function_node

    (bind
        name: (variable) @bind_name
    ) @bind_node
"#;

/// Haskell data, class, newtype, type_synonym declarations.
const QUERY_CLASSES: &str = r#"
    (class) @class_node
    (data_type) @data_type_node
    (newtype) @newtype_node
"#;

const QUERY_IMPORTS: &str = r#"
    (import) @import
"#;

/// Haskell function application.
const QUERY_CALLS: &str = r#"
    (apply
        function: (variable) @callee
    ) @apply_node
"#;

/// Type signatures as variable declarations.
const QUERY_VARIABLES: &str = r#"
    (signature
        name: (variable) @name
    ) @signature_node
"#;

/// Context types for parent context lookups in Haskell.
const FC_TYPES: &[&str] = &[
    "function",
    "bind",
    "class",
    "data_type",
    "newtype",
    "instance",
];

pub struct HaskellExtractor;

impl HaskellExtractor {
    fn execute_query<'a>(
        &self,
        query_str: &str,
        root: &'a Node<'a>,
        source: &'a [u8],
    ) -> Vec<(Node<'a>, String)> {
        let lang = self.language();
        let query = match Query::new(&lang, query_str) {
            Ok(q) => q,
            Err(_) => return Vec::new(),
        };
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, *root, source);
        let capture_names = query.capture_names();

        let mut results = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let name = &capture_names[cap.index as usize];
                results.push((cap.node, name.to_string()));
            }
        }
        results
    }

    /// Extract argument names from Haskell function patterns.
    fn extract_pattern_args(&self, node: &Node, source: &[u8]) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(patterns) = node.child_by_field_name("patterns") {
            self.collect_variables(&patterns, source, &mut args);
        }
        args
    }

    /// Recursively collect variable nodes from a pattern tree.
    fn collect_variables(&self, node: &Node, source: &[u8], names: &mut Vec<String>) {
        if node.kind() == "variable" {
            names.push(get_node_text(node, source).to_string());
            return;
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.collect_variables(&child, source, names);
            }
        }
    }
}

impl LanguageExtractor for HaskellExtractor {
    fn language(&self) -> Language {
        tree_sitter_haskell::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "haskell"
    }

    fn complexity_node_types(&self) -> &[&str] {
        COMPLEXITY_TYPES
    }

    fn find_functions(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<FunctionData> {
        let mut functions = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_FUNCTIONS, root, source) {
            match capture_name.as_str() {
                "function_node" => {
                    if node.kind() != "function" {
                        continue;
                    }
                    let key = (node.start_byte(), node.end_byte());
                    if seen.contains(&key) {
                        continue;
                    }
                    seen.insert(key);

                    let name_node = match node.child_by_field_name("name") {
                        Some(n) => n,
                        None => continue,
                    };
                    let func_name = get_node_text(&name_node, source).to_string();
                    let args = self.extract_pattern_args(&node, source);

                    let (context, context_type, _) =
                        get_parent_context(&node, source, FC_TYPES);
                    let (class_context, _, _) = get_parent_context(
                        &node,
                        source,
                        &["class", "instance"],
                    );

                    let complexity = self.calculate_complexity(&node);

                    let mut func = FunctionData {
                        name: func_name,
                        line_number: node.start_position().row + 1,
                        end_line: node.end_position().row + 1,
                        args,
                        cyclomatic_complexity: complexity,
                        context,
                        context_type,
                        class_context,
                        decorators: Vec::new(),
                        lang: self.lang_name().to_string(),
                        is_dependency: false,
                        source: None,
                        docstring: None,
                    };

                    if index_source {
                        func.source = Some(get_node_text(&node, source).to_string());
                    }

                    functions.push(func);
                }
                "bind_node" => {
                    if node.kind() != "bind" {
                        continue;
                    }
                    let key = (node.start_byte(), node.end_byte());
                    if seen.contains(&key) {
                        continue;
                    }
                    seen.insert(key);

                    let name_node = match node.child_by_field_name("name") {
                        Some(n) if n.kind() == "variable" => n,
                        _ => continue,
                    };
                    let func_name = get_node_text(&name_node, source).to_string();

                    let (context, context_type, _) =
                        get_parent_context(&node, source, FC_TYPES);
                    let (class_context, _, _) = get_parent_context(
                        &node,
                        source,
                        &["class", "instance"],
                    );

                    let mut func = FunctionData {
                        name: func_name,
                        line_number: node.start_position().row + 1,
                        end_line: node.end_position().row + 1,
                        args: Vec::new(),
                        cyclomatic_complexity: 1,
                        context,
                        context_type,
                        class_context,
                        decorators: Vec::new(),
                        lang: self.lang_name().to_string(),
                        is_dependency: false,
                        source: None,
                        docstring: None,
                    };

                    if index_source {
                        func.source = Some(get_node_text(&node, source).to_string());
                    }

                    functions.push(func);
                }
                _ => {}
            }
        }

        functions
    }

    fn find_classes(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<ClassData> {
        let mut classes = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_CLASSES, root, source) {
            let key = (node.start_byte(), node.end_byte(), node.kind());
            // Use a string key since kind() returns &str
            let hash_key = (node.start_byte(), node.end_byte());
            if seen.contains(&hash_key) {
                continue;
            }
            seen.insert(hash_key);

            let name_node = match node.child_by_field_name("name") {
                Some(n) => n,
                None => continue,
            };
            let class_name = get_node_text(&name_node, source).to_string();

            let mut class = ClassData {
                name: class_name,
                line_number: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                bases: Vec::new(),
                context: None,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&node, source).to_string());
            }

            classes.push(class);
        }

        classes
    }

    fn find_imports(&self, root: &Node, source: &[u8]) -> Vec<ImportData> {
        let mut imports = Vec::new();

        for (node, capture_name) in self.execute_query(QUERY_IMPORTS, root, source) {
            if capture_name != "import" || node.kind() != "import" {
                continue;
            }

            let module_node = node.child_by_field_name("module");
            let module_name = match module_node {
                Some(m) => get_node_text(&m, source).trim().to_string(),
                None => continue,
            };

            let alias_node = node.child_by_field_name("alias");
            let alias = alias_node.map(|a| get_node_text(&a, source).trim().to_string());

            imports.push(ImportData {
                name: module_name.clone(),
                full_import_name: module_name,
                line_number: node.start_position().row + 1,
                alias,
                context: (None, None),
                lang: self.lang_name().to_string(),
                is_dependency: false,
            });
        }

        imports
    }

    fn find_calls(&self, root: &Node, source: &[u8]) -> Vec<CallData> {
        let mut calls = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_CALLS, root, source) {
            if capture_name != "apply_node" || node.kind() != "apply" {
                continue;
            }

            let key = (node.start_byte(), node.end_byte());
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);

            let callee = match node.child_by_field_name("function") {
                Some(c) if c.kind() == "variable" => c,
                _ => continue,
            };

            let call_name = get_node_text(&callee, source).to_string();
            if call_name.is_empty() {
                continue;
            }

            let context = get_parent_context(&node, source, FC_TYPES);
            let class_ctx = get_parent_context(
                &node,
                source,
                &["class", "instance"],
            );

            calls.push(CallData {
                name: call_name.clone(),
                full_name: call_name,
                line_number: node.start_position().row + 1,
                args: Vec::new(),
                inferred_obj_type: None,
                context,
                class_context: (class_ctx.0, class_ctx.1),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                is_indirect_call: false,
            });
        }

        calls
    }

    fn find_variables(&self, root: &Node, source: &[u8]) -> Vec<VariableData> {
        let mut variables = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_VARIABLES, root, source) {
            if capture_name != "signature_node" {
                continue;
            }

            let key = (node.start_byte(), node.end_byte());
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);

            let name_node = match node.child_by_field_name("name") {
                Some(n) => n,
                None => continue,
            };
            let var_name = get_node_text(&name_node, source).to_string();

            let type_node = node.child_by_field_name("type");
            let type_annotation =
                type_node.map(|t| get_node_text(&t, source).to_string());

            let (context, _, _) = get_parent_context(&node, source, FC_TYPES);
            let (class_context, _, _) = get_parent_context(
                &node,
                source,
                &["class", "instance"],
            );

            variables.push(VariableData {
                name: var_name,
                line_number: node.start_position().row + 1,
                value: None,
                type_annotation,
                context,
                class_context,
                lang: self.lang_name().to_string(),
                is_dependency: false,
            });
        }

        variables
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_source(code: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let mut parser = Parser::new();
        let lang: Language = tree_sitter_haskell::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
add :: Int -> Int -> Int
add x y = x + y

greet :: String -> String
greet name = "Hello " ++ name
"#;
        let (tree, source) = parse_source(code);
        let ext = HaskellExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert!(funcs.len() >= 2);
    }

    #[test]
    fn test_find_classes() {
        let code = r#"
data Color = Red | Green | Blue

class Printable a where
  display :: a -> String

newtype Wrapper a = Wrapper a
"#;
        let (tree, source) = parse_source(code);
        let ext = HaskellExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.len() >= 2);
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
import Data.List
import qualified Data.Map as Map
"#;
        let (tree, source) = parse_source(code);
        let ext = HaskellExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 2);
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
factorial :: Int -> Int
factorial 0 = 1
factorial n = n * factorial (n - 1)
"#;
        let (tree, source) = parse_source(code);
        let ext = HaskellExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert!(vars.len() >= 1);
        assert_eq!(vars[0].name, "factorial");
    }
}
