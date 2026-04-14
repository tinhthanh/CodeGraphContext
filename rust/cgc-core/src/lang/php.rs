use std::collections::HashSet;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, get_parent_context, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    "if_statement",
    "for_statement",
    "foreach_statement",
    "while_statement",
    "do_statement",
    "switch_statement",
    "catch_clause",
    "conditional_expression",
];

const QUERY_FUNCTIONS: &str = r#"
    (function_definition
        name: (name) @name
        parameters: (formal_parameters) @params
    ) @function_node

    (method_declaration
        name: (name) @name
        parameters: (formal_parameters) @params
    ) @function_node
"#;

const QUERY_CLASSES: &str = r#"
    (class_declaration
        name: (name) @name
    ) @class
"#;

const QUERY_INTERFACES: &str = r#"
    (interface_declaration
        name: (name) @name
    ) @interface
"#;

const QUERY_TRAITS: &str = r#"
    (trait_declaration
        name: (name) @name
    ) @trait
"#;

const QUERY_IMPORTS: &str = r#"
    (use_declaration) @import
"#;

const QUERY_CALLS: &str = r#"
    (function_call_expression
        function: (name) @name
    )
    (function_call_expression
        function: (qualified_name) @qualified_name
    )
    (member_call_expression
        name: (name) @method_name
    )
    (scoped_call_expression
        name: (name) @scoped_name
    )
"#;

const QUERY_VARIABLES: &str = r#"
    (expression_statement
        (assignment_expression
            left: (variable_name) @name
            right: (_) @value
        )
    )
"#;

const QUERY_PRE_SCAN: &str = r#"
    (class_declaration name: (name) @name)
    (interface_declaration name: (name) @name)
    (trait_declaration name: (name) @name)
    (function_definition name: (name) @name)
    (method_declaration name: (name) @name)
"#;

pub struct PhpExtractor;

impl PhpExtractor {
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

    fn get_parent_context_php(
        &self,
        node: &Node,
        source: &[u8],
    ) -> (Option<String>, Option<String>, Option<usize>) {
        let types = &[
            "function_definition",
            "method_declaration",
            "class_declaration",
            "interface_declaration",
            "trait_declaration",
        ];
        get_parent_context(node, source, types)
    }

    fn get_class_context_php(&self, node: &Node, source: &[u8]) -> Option<String> {
        let types = &[
            "class_declaration",
            "interface_declaration",
            "trait_declaration",
        ];
        let (name, _, _) = get_parent_context(node, source, types);
        name
    }

    fn extract_php_params(&self, params_node: &Node, source: &[u8]) -> Vec<String> {
        let mut params = Vec::new();
        for i in 0..params_node.child_count() {
            let child = match params_node.child(i) {
                Some(c) => c,
                None => continue,
            };
            if child.kind() == "simple_parameter" || child.kind() == "variadic_parameter" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    params.push(get_node_text(&name_node, source).to_string());
                }
            }
        }
        params
    }

    fn extract_base_classes(&self, class_node: &Node, source: &[u8]) -> Vec<String> {
        let mut bases = Vec::new();

        // Look for base_clause (extends) and class_interface_clause (implements)
        if let Some(base_clause) = class_node.child_by_field_name("base_clause") {
            for i in 0..base_clause.child_count() {
                if let Some(child) = base_clause.child(i) {
                    if child.kind() == "name" || child.kind() == "qualified_name" {
                        bases.push(get_node_text(&child, source).to_string());
                    }
                }
            }
        }

        // Check for interfaces_clause (implements)
        for i in 0..class_node.child_count() {
            if let Some(child) = class_node.child(i) {
                if child.kind() == "class_interface_clause" {
                    for j in 0..child.child_count() {
                        if let Some(iface) = child.child(j) {
                            if iface.kind() == "name" || iface.kind() == "qualified_name" {
                                bases.push(get_node_text(&iface, source).to_string());
                            }
                        }
                    }
                }
            }
        }

        bases
    }
}

impl LanguageExtractor for PhpExtractor {
    fn language(&self) -> Language {
        tree_sitter_php::LANGUAGE_PHP.into()
    }

    fn lang_name(&self) -> &str {
        "php"
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
        let mut seen_nodes: HashSet<(usize, usize)> = HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_FUNCTIONS, root, source) {
            if capture_name != "name" {
                continue;
            }

            let func_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };

            if !matches!(
                func_node.kind(),
                "function_definition" | "method_declaration"
            ) {
                continue;
            }

            let node_id = (func_node.start_byte(), func_node.end_byte());
            if seen_nodes.contains(&node_id) {
                continue;
            }
            seen_nodes.insert(node_id);

            let name = get_node_text(&node, source).to_string();
            let params_node = func_node.child_by_field_name("parameters");
            let args = match params_node {
                Some(ref pn) => self.extract_php_params(pn, source),
                None => Vec::new(),
            };

            let complexity = self.calculate_complexity(&func_node);
            let (context, context_type, _) =
                self.get_parent_context_php(&func_node, source);
            let class_context = self.get_class_context_php(&func_node, source);

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

        functions
    }

    fn find_classes(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<ClassData> {
        let mut classes = Vec::new();

        // Classes
        for (node, capture_name) in self.execute_query(QUERY_CLASSES, root, source) {
            if capture_name != "name" {
                continue;
            }
            let class_node = match node.parent() {
                Some(p) if p.kind() == "class_declaration" => p,
                _ => continue,
            };
            let name = get_node_text(&node, source).to_string();
            let bases = self.extract_base_classes(&class_node, source);

            let mut class = ClassData {
                name,
                line_number: class_node.start_position().row + 1,
                end_line: class_node.end_position().row + 1,
                bases,
                context: None,
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

        // Interfaces
        for (node, capture_name) in self.execute_query(QUERY_INTERFACES, root, source) {
            if capture_name != "name" {
                continue;
            }
            let iface_node = match node.parent() {
                Some(p) if p.kind() == "interface_declaration" => p,
                _ => continue,
            };
            let name = get_node_text(&node, source).to_string();

            let mut class = ClassData {
                name,
                line_number: iface_node.start_position().row + 1,
                end_line: iface_node.end_position().row + 1,
                bases: Vec::new(),
                context: None,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&iface_node, source).to_string());
            }

            classes.push(class);
        }

        // Traits
        for (node, capture_name) in self.execute_query(QUERY_TRAITS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let trait_node = match node.parent() {
                Some(p) if p.kind() == "trait_declaration" => p,
                _ => continue,
            };
            let name = get_node_text(&node, source).to_string();

            let mut class = ClassData {
                name,
                line_number: trait_node.start_position().row + 1,
                end_line: trait_node.end_position().row + 1,
                bases: Vec::new(),
                context: None,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&trait_node, source).to_string());
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

            let import_text = get_node_text(&node, source).to_string();

            // Parse: use Foo\Bar\Baz;
            // or: use Foo\Bar as Alias;
            let stripped = import_text
                .trim_start_matches("use")
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_string();

            let (import_path, alias) = if stripped.contains(" as ") {
                let parts: Vec<&str> = stripped.splitn(2, " as ").collect();
                (
                    parts[0].trim().to_string(),
                    Some(parts[1].trim().to_string()),
                )
            } else {
                (stripped, None)
            };

            // Extract short name from full path
            let short_name = import_path
                .rsplit('\\')
                .next()
                .unwrap_or(&import_path)
                .to_string();

            imports.push(ImportData {
                name: short_name,
                full_import_name: import_path,
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
        let mut seen: HashSet<(String, usize)> = HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_CALLS, root, source) {
            let call_name = get_node_text(&node, source).to_string();
            let line_number = node.start_position().row + 1;

            let call_key = (call_name.clone(), line_number);
            if seen.contains(&call_key) {
                continue;
            }
            seen.insert(call_key);

            // Determine full name based on capture type
            let full_name = match capture_name.as_str() {
                "method_name" => {
                    // member_call_expression: $obj->method()
                    if let Some(call_expr) = node.parent() {
                        if let Some(obj_node) = call_expr.child_by_field_name("object") {
                            let receiver = get_node_text(&obj_node, source);
                            format!("{}.{}", receiver, call_name)
                        } else {
                            call_name.clone()
                        }
                    } else {
                        call_name.clone()
                    }
                }
                "scoped_name" => {
                    // scoped_call_expression: Class::method()
                    if let Some(call_expr) = node.parent() {
                        if let Some(scope_node) = call_expr.child_by_field_name("scope") {
                            let scope = get_node_text(&scope_node, source);
                            format!("{}.{}", scope, call_name)
                        } else {
                            call_name.clone()
                        }
                    } else {
                        call_name.clone()
                    }
                }
                _ => call_name.clone(),
            };

            let (context_name, context_type, context_line) =
                self.get_parent_context_php(&node, source);
            let class_context = self.get_class_context_php(&node, source);

            calls.push(CallData {
                name: call_name,
                full_name,
                line_number,
                args: Vec::new(),
                inferred_obj_type: None,
                context: (context_name, context_type, context_line),
                class_context: (class_context, None),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                is_indirect_call: false,
            });
        }

        calls
    }

    fn find_variables(&self, root: &Node, source: &[u8]) -> Vec<VariableData> {
        let mut variables = Vec::new();
        let mut seen: HashSet<usize> = HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_VARIABLES, root, source) {
            if capture_name != "name" {
                continue;
            }

            let start_byte = node.start_byte();
            if seen.contains(&start_byte) {
                continue;
            }
            seen.insert(start_byte);

            let name = get_node_text(&node, source).to_string();

            // Walk up to assignment for value
            let value = node.parent().and_then(|assignment| {
                assignment
                    .child_by_field_name("right")
                    .map(|v| get_node_text(&v, source).to_string())
            });

            let (context, _, _) = self.get_parent_context_php(&node, source);
            let class_context = self.get_class_context_php(&node, source);

            variables.push(VariableData {
                name,
                line_number: node.start_position().row + 1,
                value,
                type_annotation: None,
                context,
                class_context,
                lang: self.lang_name().to_string(),
                is_dependency: false,
            });
        }

        variables
    }

    fn pre_scan_definitions(&self, root: &Node, source: &[u8]) -> Vec<String> {
        let mut names = Vec::new();
        for (node, _) in self.execute_query(QUERY_PRE_SCAN, root, source) {
            names.push(get_node_text(&node, source).to_string());
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_source(code: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let mut parser = Parser::new();
        let lang: Language = tree_sitter_php::LANGUAGE_PHP.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"<?php
function greet($name) {
    echo "Hello $name";
}

class Calculator {
    public function add($a, $b) {
        return $a + $b;
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = PhpExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert!(funcs.len() >= 2);
        assert!(funcs.iter().any(|f| f.name == "greet"));
        assert!(funcs.iter().any(|f| f.name == "add"));
    }

    #[test]
    fn test_find_classes_and_interfaces() {
        let code = r#"<?php
interface Loggable {
    public function log($message);
}

class FileLogger implements Loggable {
    public function log($message) {
        file_put_contents('log.txt', $message);
    }
}

trait Singleton {
    private static $instance;
}
"#;
        let (tree, source) = parse_source(code);
        let ext = PhpExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.iter().any(|c| c.name == "Loggable"));
        assert!(classes.iter().any(|c| c.name == "FileLogger"));
        assert!(classes.iter().any(|c| c.name == "Singleton"));
    }

    #[test]
    fn test_find_calls() {
        let code = r#"<?php
function test() {
    echo strlen("hello");
    array_push($arr, 1);
}
"#;
        let (tree, source) = parse_source(code);
        let ext = PhpExtractor;
        let calls = ext.find_calls(&tree.root_node(), &source);
        assert!(calls.iter().any(|c| c.name == "strlen"));
        assert!(calls.iter().any(|c| c.name == "array_push"));
    }

    #[test]
    fn test_complexity() {
        let code = r#"<?php
function complex($x) {
    if ($x > 0) {
        for ($i = 0; $i < $x; $i++) {
            if ($i % 2 == 0) {
                while ($x > 0) {
                    $x--;
                }
            }
        }
    }
    return $x;
}
"#;
        let (tree, source) = parse_source(code);
        let ext = PhpExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 1);
        assert!(funcs[0].cyclomatic_complexity >= 4);
    }
}
