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
    (method_declaration
        name: (identifier) @name
        parameters: (parameter_list) @params
    ) @function_node

    (constructor_declaration
        name: (identifier) @name
        parameters: (parameter_list) @params
    ) @function_node

    (local_function_statement
        name: (identifier) @name
        parameters: (parameter_list) @params
    ) @function_node
"#;

const QUERY_CLASSES: &str = r#"
    (class_declaration
        name: (identifier) @name
    ) @class
"#;

const QUERY_INTERFACES: &str = r#"
    (interface_declaration
        name: (identifier) @name
    ) @interface
"#;

const QUERY_STRUCTS: &str = r#"
    (struct_declaration
        name: (identifier) @name
    ) @struct
"#;

const QUERY_ENUMS: &str = r#"
    (enum_declaration
        name: (identifier) @name
    ) @enum
"#;

const QUERY_IMPORTS: &str = r#"
    (using_directive) @import
"#;

const QUERY_CALLS: &str = r#"
    (invocation_expression
        function: (identifier) @name
    )
    (invocation_expression
        function: (member_access_expression
            name: (identifier) @name
        )
    )
    (object_creation_expression
        type: (identifier) @ctor_name
    )
"#;

const QUERY_VARIABLES: &str = r#"
    (variable_declaration
        (variable_declarator
            (identifier) @name
        )
    )
"#;

const QUERY_PRE_SCAN: &str = r#"
    (class_declaration name: (identifier) @name)
    (interface_declaration name: (identifier) @name)
    (struct_declaration name: (identifier) @name)
    (enum_declaration name: (identifier) @name)
    (method_declaration name: (identifier) @name)
"#;

pub struct CSharpExtractor;

impl CSharpExtractor {
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

    fn get_parent_context_csharp(
        &self,
        node: &Node,
        source: &[u8],
    ) -> (Option<String>, Option<String>, Option<usize>) {
        let types = &[
            "method_declaration",
            "constructor_declaration",
            "class_declaration",
            "struct_declaration",
            "interface_declaration",
        ];
        get_parent_context(node, source, types)
    }

    fn get_class_context_csharp(&self, node: &Node, source: &[u8]) -> Option<String> {
        let types = &[
            "class_declaration",
            "struct_declaration",
            "interface_declaration",
        ];
        let (name, _, _) = get_parent_context(node, source, types);
        name
    }

    fn extract_params(&self, params_node: &Node, source: &[u8]) -> Vec<String> {
        let mut params = Vec::new();
        for i in 0..params_node.child_count() {
            let child = match params_node.child(i) {
                Some(c) => c,
                None => continue,
            };
            if child.kind() == "parameter" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    params.push(get_node_text(&name_node, source).to_string());
                } else {
                    // Fallback: find first identifier child
                    for j in 0..child.child_count() {
                        if let Some(sub) = child.child(j) {
                            if sub.kind() == "identifier" {
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

    fn extract_base_list(&self, type_node: &Node, source: &[u8]) -> Vec<String> {
        let mut bases = Vec::new();
        for i in 0..type_node.child_count() {
            if let Some(child) = type_node.child(i) {
                if child.kind() == "base_list" {
                    for j in 0..child.child_count() {
                        if let Some(base) = child.child(j) {
                            if base.kind() == "identifier"
                                || base.kind() == "qualified_name"
                                || base.kind() == "generic_name"
                            {
                                bases.push(get_node_text(&base, source).to_string());
                            }
                        }
                    }
                    break;
                }
            }
        }
        bases
    }
}

impl LanguageExtractor for CSharpExtractor {
    fn language(&self) -> Language {
        tree_sitter_c_sharp::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "c_sharp"
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

            // Ensure it's a function-like node
            if !matches!(
                func_node.kind(),
                "method_declaration"
                    | "constructor_declaration"
                    | "local_function_statement"
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
                Some(ref pn) => self.extract_params(pn, source),
                None => Vec::new(),
            };

            let complexity = self.calculate_complexity(&func_node);
            let (context, context_type, _) =
                self.get_parent_context_csharp(&func_node, source);
            let class_context = self.get_class_context_csharp(&func_node, source);

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
            let bases = self.extract_base_list(&class_node, source);
            let (context, _, _) = self.get_parent_context_csharp(&class_node, source);

            let mut class = ClassData {
                name,
                line_number: class_node.start_position().row + 1,
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
            let bases = self.extract_base_list(&iface_node, source);

            let mut class = ClassData {
                name,
                line_number: iface_node.start_position().row + 1,
                end_line: iface_node.end_position().row + 1,
                bases,
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

        // Structs
        for (node, capture_name) in self.execute_query(QUERY_STRUCTS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let struct_node = match node.parent() {
                Some(p) if p.kind() == "struct_declaration" => p,
                _ => continue,
            };
            let name = get_node_text(&node, source).to_string();
            let bases = self.extract_base_list(&struct_node, source);

            let mut class = ClassData {
                name,
                line_number: struct_node.start_position().row + 1,
                end_line: struct_node.end_position().row + 1,
                bases,
                context: None,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&struct_node, source).to_string());
            }

            classes.push(class);
        }

        // Enums
        for (node, capture_name) in self.execute_query(QUERY_ENUMS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let enum_node = match node.parent() {
                Some(p) if p.kind() == "enum_declaration" => p,
                _ => continue,
            };
            let name = get_node_text(&node, source).to_string();

            let mut class = ClassData {
                name,
                line_number: enum_node.start_position().row + 1,
                end_line: enum_node.end_position().row + 1,
                bases: Vec::new(),
                context: None,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&enum_node, source).to_string());
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

            // Parse: using System.Collections.Generic;
            // or: using static System.Math;
            // or: using MyAlias = System.Collections.Generic.List<int>;
            let stripped = import_text
                .trim_start_matches("using")
                .trim()
                .trim_start_matches("static")
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_string();

            let (import_path, alias) = if stripped.contains('=') {
                let parts: Vec<&str> = stripped.splitn(2, '=').collect();
                (
                    parts[1].trim().to_string(),
                    Some(parts[0].trim().to_string()),
                )
            } else {
                (stripped, None)
            };

            imports.push(ImportData {
                name: import_path.clone(),
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
            if capture_name != "name" && capture_name != "ctor_name" {
                continue;
            }

            let call_name = get_node_text(&node, source).to_string();
            let line_number = node.start_position().row + 1;

            let call_key = (call_name.clone(), line_number);
            if seen.contains(&call_key) {
                continue;
            }
            seen.insert(call_key);

            let (context_name, context_type, context_line) =
                self.get_parent_context_csharp(&node, source);
            let class_context = self.get_class_context_csharp(&node, source);

            calls.push(CallData {
                name: call_name.clone(),
                full_name: call_name,
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

            // Walk up to variable_declaration for type info
            let mut decl_node = node.parent();
            while let Some(d) = decl_node {
                if d.kind() == "variable_declaration" {
                    break;
                }
                decl_node = d.parent();
            }

            let type_annotation = decl_node.and_then(|d| {
                d.child_by_field_name("type")
                    .map(|t| get_node_text(&t, source).to_string())
            });

            // Get value from variable_declarator
            let value = node.parent().and_then(|p| {
                if p.kind() == "variable_declarator" {
                    // Look for equals_value_clause
                    for i in 0..p.child_count() {
                        if let Some(child) = p.child(i) {
                            if child.kind() == "equals_value_clause" {
                                // The value is the child of equals_value_clause
                                return child
                                    .child(1)
                                    .map(|v| get_node_text(&v, source).to_string());
                            }
                        }
                    }
                }
                None
            });

            let (context, _, _) = self.get_parent_context_csharp(&node, source);
            let class_context = self.get_class_context_csharp(&node, source);

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
        let lang: Language = tree_sitter_c_sharp::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
class Calculator {
    public int Add(int a, int b) {
        return a + b;
    }

    public Calculator() {
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = CSharpExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert!(funcs.len() >= 2);
        assert!(funcs.iter().any(|f| f.name == "Add"));
        assert!(funcs.iter().any(|f| f.name == "Calculator"));
    }

    #[test]
    fn test_find_classes_and_interfaces() {
        let code = r#"
using System;

interface IAnimal {
    void Speak();
}

class Dog : IAnimal {
    public void Speak() {}
}

struct Point {
    public int X;
    public int Y;
}

enum Color {
    Red,
    Green,
    Blue
}
"#;
        let (tree, source) = parse_source(code);
        let ext = CSharpExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.iter().any(|c| c.name == "IAnimal"));
        assert!(classes.iter().any(|c| c.name == "Dog"));
        assert!(classes.iter().any(|c| c.name == "Point"));
        assert!(classes.iter().any(|c| c.name == "Color"));
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
using System;
using System.Collections.Generic;
using System.Linq;
"#;
        let (tree, source) = parse_source(code);
        let ext = CSharpExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 3);
        assert!(imports.iter().any(|i| i.name == "System"));
    }

    #[test]
    fn test_find_calls() {
        let code = r#"
class Test {
    void Run() {
        Console.WriteLine("hello");
        var obj = new MyClass();
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = CSharpExtractor;
        let calls = ext.find_calls(&tree.root_node(), &source);
        assert!(calls.len() >= 1);
    }

    #[test]
    fn test_complexity() {
        let code = r#"
class Test {
    int Complex(int x) {
        if (x > 0) {
            for (int i = 0; i < x; i++) {
                if (i % 2 == 0) {
                    while (x > 0) {
                        x--;
                    }
                }
            }
        }
        return x;
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = CSharpExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 1);
        assert!(funcs[0].cyclomatic_complexity >= 4);
    }
}
