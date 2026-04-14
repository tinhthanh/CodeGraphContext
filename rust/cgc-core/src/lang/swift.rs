use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, get_parent_context, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    "if_statement",
    "for_statement",
    "while_statement",
    "switch_statement",
    "guard_statement",
    "catch_clause",
];

const QUERY_FUNCTIONS: &str = r#"
    (function_declaration
        name: (simple_identifier) @name
    ) @function_node

    (init_declaration) @init_node
"#;

const QUERY_CLASSES: &str = r#"
    (class_declaration
        name: (type_identifier) @name
    ) @class_node

    (protocol_declaration
        name: (type_identifier) @name
    ) @class_node
"#;

const QUERY_IMPORTS: &str = r#"
    (import_declaration) @import
"#;

const QUERY_CALLS: &str = r#"
    (call_expression
        (simple_identifier) @name
    ) @call_node
"#;

const QUERY_VARIABLES: &str = r#"
    (property_declaration) @variable
"#;

/// Context types used for parent context lookups in Swift.
const FC_TYPES: &[&str] = &[
    "function_declaration",
    "init_declaration",
    "class_declaration",
    "protocol_declaration",
];

pub struct SwiftExtractor;

impl SwiftExtractor {
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

    /// Extract parameter names from a Swift function declaration.
    fn extract_parameters(&self, func_node: &Node, source: &[u8]) -> Vec<String> {
        let mut params = Vec::new();
        for i in 0..func_node.child_count() {
            if let Some(child) = func_node.child(i) {
                if child.kind() == "parameter" {
                    for j in 0..child.child_count() {
                        if let Some(sub) = child.child(j) {
                            if sub.kind() == "simple_identifier" {
                                params.push(get_node_text(&sub, source).to_string());
                                break;
                            }
                        }
                    }
                }
            }
        }
        params
    }
}

impl LanguageExtractor for SwiftExtractor {
    fn language(&self) -> Language {
        tree_sitter_swift::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "swift"
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
                    let key = (node.start_byte(), node.end_byte());
                    if seen.contains(&key) {
                        continue;
                    }
                    seen.insert(key);

                    // Find name child
                    let func_name = {
                        let mut name = None;
                        for i in 0..node.child_count() {
                            if let Some(child) = node.child(i) {
                                if child.kind() == "simple_identifier" {
                                    name = Some(get_node_text(&child, source).to_string());
                                    break;
                                }
                            }
                        }
                        match name {
                            Some(n) => n,
                            None => continue,
                        }
                    };

                    let args = self.extract_parameters(&node, source);

                    let (context, context_type, _) =
                        get_parent_context(&node, source, FC_TYPES);
                    let (class_context, _, _) = get_parent_context(
                        &node,
                        source,
                        &["class_declaration", "protocol_declaration"],
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
                "init_node" => {
                    let key = (node.start_byte(), node.end_byte());
                    if seen.contains(&key) {
                        continue;
                    }
                    seen.insert(key);

                    let args = self.extract_parameters(&node, source);

                    let (context, context_type, _) =
                        get_parent_context(&node, source, FC_TYPES);
                    let (class_context, _, _) = get_parent_context(
                        &node,
                        source,
                        &["class_declaration", "protocol_declaration"],
                    );

                    let complexity = self.calculate_complexity(&node);

                    let mut func = FunctionData {
                        name: "init".to_string(),
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
            if capture_name != "name" {
                continue;
            }
            let class_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };

            let key = (class_node.start_byte(), class_node.end_byte());
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);

            let name = get_node_text(&node, source).to_string();

            // Extract inheritance
            let mut bases = Vec::new();
            for i in 0..class_node.child_count() {
                if let Some(child) = class_node.child(i) {
                    if child.kind() == "type_inheritance_clause" {
                        for j in 0..child.child_count() {
                            if let Some(sub) = child.child(j) {
                                if sub.kind() == "type_identifier" {
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
            let module = text
                .strip_prefix("import ")
                .unwrap_or(text)
                .trim()
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();

            if !module.is_empty() {
                imports.push(ImportData {
                    name: module.clone(),
                    full_import_name: module,
                    line_number: node.start_position().row + 1,
                    alias: None,
                    context: (None, None),
                    lang: self.lang_name().to_string(),
                    is_dependency: false,
                });
            }
        }

        imports
    }

    fn find_calls(&self, root: &Node, source: &[u8]) -> Vec<CallData> {
        let mut calls = Vec::new();

        for (node, capture_name) in self.execute_query(QUERY_CALLS, root, source) {
            if capture_name != "name" {
                continue;
            }

            match node.parent() {
                Some(p) if p.kind() == "call_expression" => p,
                _ => continue,
            };

            let call_name = get_node_text(&node, source).to_string();

            let context = get_parent_context(&node, source, FC_TYPES);
            let class_ctx = get_parent_context(
                &node,
                source,
                &["class_declaration", "protocol_declaration"],
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

        for (node, capture_name) in self.execute_query(QUERY_VARIABLES, root, source) {
            if capture_name != "variable" {
                continue;
            }

            // Find the identifier name within the property declaration
            let mut var_name = None;
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "pattern" {
                        // pattern -> bound_identifier -> simple_identifier
                        for j in 0..child.child_count() {
                            if let Some(sub) = child.child(j) {
                                if sub.kind() == "simple_identifier" || sub.kind() == "bound_identifier" {
                                    var_name = Some(get_node_text(&sub, source).to_string());
                                    break;
                                }
                            }
                        }
                        if var_name.is_none() {
                            var_name = Some(get_node_text(&child, source).to_string());
                        }
                        break;
                    } else if child.kind() == "simple_identifier" {
                        var_name = Some(get_node_text(&child, source).to_string());
                        break;
                    }
                }
            }

            let name = match var_name {
                Some(n) if !n.is_empty() => n,
                _ => continue,
            };

            // Type annotation
            let mut type_annotation = None;
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "type_annotation" {
                        for j in 0..child.child_count() {
                            if let Some(sub) = child.child(j) {
                                if sub.kind() == "type_identifier" {
                                    type_annotation =
                                        Some(get_node_text(&sub, source).to_string());
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            let (context, _, _) = get_parent_context(&node, source, FC_TYPES);
            let (class_context, _, _) = get_parent_context(
                &node,
                source,
                &["class_declaration", "protocol_declaration"],
            );

            variables.push(VariableData {
                name,
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
        let lang: Language = tree_sitter_swift::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
func greet(name: String) -> String {
    return "Hello \(name)"
}

func add(a: Int, b: Int) -> Int {
    return a + b
}
"#;
        let (tree, source) = parse_source(code);
        let ext = SwiftExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert!(funcs.len() >= 2);
        assert_eq!(funcs[0].name, "greet");
        assert_eq!(funcs[1].name, "add");
    }

    #[test]
    fn test_find_classes() {
        let code = r#"
class Animal {
    var name: String
    init(name: String) {
        self.name = name
    }
}

protocol Greetable {
    func greet() -> String
}
"#;
        let (tree, source) = parse_source(code);
        let ext = SwiftExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.len() >= 2);
        assert_eq!(classes[0].name, "Animal");
        assert_eq!(classes[1].name, "Greetable");
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
import Foundation
import UIKit
"#;
        let (tree, source) = parse_source(code);
        let ext = SwiftExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 2);
        assert_eq!(imports[0].name, "Foundation");
        assert_eq!(imports[1].name, "UIKit");
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
class MyClass {
    var count: Int = 0
    let name: String = "test"
}
"#;
        let (tree, source) = parse_source(code);
        let ext = SwiftExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert!(vars.len() >= 2);
    }
}
