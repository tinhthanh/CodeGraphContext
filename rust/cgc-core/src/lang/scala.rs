use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, get_parent_context, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    "if_expression",
    "for_expression",
    "while_expression",
    "match_expression",
    "case_clause",
    "catch_clause",
];

const QUERY_FUNCTIONS: &str = r#"
    (function_definition
        name: (identifier) @name
        parameters: (parameters) @params
    ) @function_node
"#;

const QUERY_LAMBDA_VALS: &str = r#"
    (val_definition
        pattern: (identifier) @name
        value: (lambda_expression) @lambda_node
    )
"#;

const QUERY_CLASSES: &str = r#"
    (class_definition
        name: (identifier) @name
    ) @class_node

    (trait_definition
        name: (identifier) @name
    ) @class_node

    (object_definition
        name: (identifier) @name
    ) @class_node
"#;

const QUERY_IMPORTS: &str = r#"
    (import_declaration) @import
"#;

const QUERY_CALLS: &str = r#"
    (call_expression
        function: (identifier) @name
    ) @call_node

    (call_expression
        function: (field_expression
            field: (identifier) @name
        ) @full_call
    ) @call_node
"#;

const QUERY_VARIABLES: &str = r#"
    (val_definition
        pattern: (identifier) @name
    ) @variable

    (var_definition
        pattern: (identifier) @name
    ) @variable
"#;

/// Context types used for parent context lookups in Scala.
const FC_TYPES: &[&str] = &[
    "function_definition",
    "class_definition",
    "trait_definition",
    "object_definition",
];

pub struct ScalaExtractor;

impl ScalaExtractor {
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

    /// Extract parameter names from a Scala parameters node.
    fn extract_parameters(&self, params_node: &Node, source: &[u8]) -> Vec<String> {
        let text = get_node_text(params_node, source);
        let clean = text.trim_start_matches('(').trim_end_matches(')');
        if clean.is_empty() {
            return Vec::new();
        }
        clean
            .split(',')
            .filter_map(|p| {
                let trimmed = p.trim();
                if trimmed.is_empty() {
                    return None;
                }
                // "name: Type" -> extract name
                let name_part = if let Some(idx) = trimmed.find(':') {
                    trimmed[..idx].trim()
                } else {
                    trimmed
                };
                // Remove modifiers like implicit, override, etc.
                let tokens: Vec<&str> = name_part.split_whitespace().collect();
                tokens.last().map(|s| s.to_string())
            })
            .collect()
    }
}

impl LanguageExtractor for ScalaExtractor {
    fn language(&self) -> Language {
        tree_sitter_scala::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "scala"
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

        for (node, capture_name) in self.execute_query(QUERY_FUNCTIONS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let func_node = match node.parent() {
                Some(p) if p.kind() == "function_definition" => p,
                _ => continue,
            };
            let name = get_node_text(&node, source).to_string();
            let params_node = func_node.child_by_field_name("parameters");

            let args = match params_node {
                Some(ref pn) => self.extract_parameters(pn, source),
                None => Vec::new(),
            };

            let (context, context_type, _) = get_parent_context(&func_node, source, FC_TYPES);
            let (class_context, _, _) = get_parent_context(
                &func_node,
                source,
                &["class_definition", "trait_definition", "object_definition"],
            );

            let complexity = self.calculate_complexity(&func_node);

            let mut func = FunctionData {
                name,
                line_number: node.start_position().row + 1,
                end_line: func_node.end_position().row + 1,
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
                func.source = Some(get_node_text(&func_node, source).to_string());
            }

            functions.push(func);
        }

        // Lambda val assignments
        for (node, capture_name) in self.execute_query(QUERY_LAMBDA_VALS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let val_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };
            let name = get_node_text(&node, source).to_string();
            let (context, context_type, _) = get_parent_context(&val_node, source, FC_TYPES);
            let (class_context, _, _) = get_parent_context(
                &val_node,
                source,
                &["class_definition", "trait_definition", "object_definition"],
            );

            let mut func = FunctionData {
                name,
                line_number: node.start_position().row + 1,
                end_line: val_node.end_position().row + 1,
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
                func.source = Some(get_node_text(&val_node, source).to_string());
            }

            functions.push(func);
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

        for (node, capture_name) in self.execute_query(QUERY_CLASSES, root, source) {
            if capture_name != "name" {
                continue;
            }
            let class_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };
            let name = get_node_text(&node, source).to_string();

            // Extract bases from extends clause
            let mut bases = Vec::new();
            for i in 0..class_node.child_count() {
                if let Some(child) = class_node.child(i) {
                    if child.kind() == "extends_clause" {
                        for j in 0..child.child_count() {
                            if let Some(sub) = child.child(j) {
                                if sub.kind() == "type_identifier" || sub.kind() == "user_type" {
                                    bases.push(get_node_text(&sub, source).to_string());
                                }
                            }
                        }
                    }
                }
            }

            let (context, _, _) = get_parent_context(&class_node, source, FC_TYPES);

            let mut class = ClassData {
                name,
                line_number: node.start_position().row + 1,
                end_line: class_node.end_position().row + 1,
                bases,
                context,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&class_node, source).to_string());
            }

            classes.push(class);
        }

        classes
    }

    fn find_imports(&self, root: &Node, source: &[u8]) -> Vec<ImportData> {
        let mut imports = Vec::new();

        for (node, capture_name) in self.execute_query(QUERY_IMPORTS, root, source) {
            if capture_name != "import" {
                continue;
            }
            let text = get_node_text(&node, source);
            let clean = text.strip_prefix("import ").unwrap_or(text).trim();

            imports.push(ImportData {
                name: clean.to_string(),
                full_import_name: clean.to_string(),
                line_number: node.start_position().row + 1,
                alias: None,
                context: (None, None),
                lang: self.lang_name().to_string(),
                is_dependency: false,
            });
        }

        imports
    }

    fn find_calls(&self, root: &Node, source: &[u8]) -> Vec<CallData> {
        let mut calls = Vec::new();

        for (node, capture_name) in self.execute_query(QUERY_CALLS, root, source) {
            if capture_name != "name" {
                continue;
            }

            // Navigate to the call_expression node
            let call_node = {
                let parent = match node.parent() {
                    Some(p) => p,
                    None => continue,
                };
                if parent.kind() == "call_expression" {
                    parent
                } else {
                    match parent.parent() {
                        Some(gp) if gp.kind() == "call_expression" => gp,
                        _ => continue,
                    }
                }
            };

            let full_call_node = match call_node.child_by_field_name("function") {
                Some(n) => n,
                None => continue,
            };

            let context = get_parent_context(&node, source, FC_TYPES);
            let class_ctx = get_parent_context(
                &node,
                source,
                &["class_definition", "trait_definition", "object_definition"],
            );

            calls.push(CallData {
                name: get_node_text(&node, source).to_string(),
                full_name: get_node_text(&full_call_node, source).to_string(),
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

        for (node, capture_name) in self.execute_query(QUERY_VARIABLES, root, source) {
            if capture_name != "name" {
                continue;
            }

            let def_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };

            // Skip lambda assignments (handled as functions)
            if let Some(value) = def_node.child_by_field_name("value") {
                if value.kind() == "lambda_expression" {
                    continue;
                }
            }

            let name = get_node_text(&node, source).to_string();
            let value = def_node
                .child_by_field_name("value")
                .map(|n| get_node_text(&n, source).to_string());
            let type_annotation = def_node
                .child_by_field_name("type")
                .map(|n| get_node_text(&n, source).to_string());

            let (context, _, _) = get_parent_context(&node, source, FC_TYPES);
            let (class_context, _, _) = get_parent_context(
                &node,
                source,
                &["class_definition", "trait_definition", "object_definition"],
            );

            variables.push(VariableData {
                name,
                line_number: node.start_position().row + 1,
                value,
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
        let lang: Language = tree_sitter_scala::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
def add(a: Int, b: Int): Int = a + b

def greet(name: String): Unit = {
  println(s"Hello $name")
}
"#;
        let (tree, source) = parse_source(code);
        let ext = ScalaExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert!(funcs.len() >= 2);
        assert_eq!(funcs[0].name, "add");
        assert_eq!(funcs[1].name, "greet");
    }

    #[test]
    fn test_find_classes() {
        let code = r#"
class Animal(name: String)

trait Greetable {
  def greet(): String
}

object Main {
  def main(args: Array[String]): Unit = {}
}
"#;
        let (tree, source) = parse_source(code);
        let ext = ScalaExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.len() >= 3);
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
import scala.collection.mutable
import java.util.{Date, List}
"#;
        let (tree, source) = parse_source(code);
        let ext = ScalaExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 2);
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
val x: Int = 42
var name = "hello"
"#;
        let (tree, source) = parse_source(code);
        let ext = ScalaExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert!(vars.len() >= 2);
        assert_eq!(vars[0].name, "x");
    }
}
