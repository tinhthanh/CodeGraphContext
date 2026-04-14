use std::collections::HashSet;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    "if_expression",
    "for_expression",
    "while_expression",
    "match_expression",
    "match_arm",
    "boolean_operator",
];

const QUERY_FUNCTIONS: &str = r#"
    (function_item
        name: (identifier) @name
        parameters: (parameters) @params
    ) @function_node
"#;

const QUERY_STRUCTS: &str = r#"
    (struct_item
        name: (type_identifier) @name
    ) @struct
"#;

const QUERY_ENUMS: &str = r#"
    (enum_item
        name: (type_identifier) @name
    ) @enum
"#;

const QUERY_TRAITS: &str = r#"
    (trait_item
        name: (type_identifier) @name
    ) @trait
"#;

const QUERY_IMPORTS: &str = r#"
    (use_declaration) @import
"#;

const QUERY_CALLS: &str = r#"
    (call_expression
        function: (identifier) @name
    )
    (call_expression
        function: (field_expression
            field: (field_identifier) @name
        )
    )
    (call_expression
        function: (scoped_identifier
            name: (identifier) @name
        )
    )
    (macro_invocation
        macro: (identifier) @macro_name
    )
"#;

const QUERY_VARIABLES: &str = r#"
    (let_declaration
        pattern: (identifier) @name
    )
"#;

const QUERY_PRE_SCAN: &str = r#"
    (function_item name: (identifier) @name)
    (struct_item name: (type_identifier) @name)
    (enum_item name: (type_identifier) @name)
    (trait_item name: (type_identifier) @name)
"#;

pub struct RustExtractor;

impl RustExtractor {
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

    fn get_parent_context_rust(
        &self,
        node: &Node,
        source: &[u8],
    ) -> (Option<String>, Option<String>, Option<usize>) {
        let mut curr = node.parent();
        while let Some(parent) = curr {
            match parent.kind() {
                "function_item" => {
                    let name_node = parent.child_by_field_name("name");
                    return (
                        name_node.map(|n| get_node_text(&n, source).to_string()),
                        Some(parent.kind().to_string()),
                        Some(parent.start_position().row + 1),
                    );
                }
                "impl_item" => {
                    // For impl blocks, find the type name
                    let mut type_name = None;
                    for i in 0..parent.child_count() {
                        if let Some(child) = parent.child(i) {
                            if child.kind() == "type_identifier" {
                                type_name = Some(get_node_text(&child, source).to_string());
                            }
                        }
                    }
                    return (
                        type_name,
                        Some("impl_item".to_string()),
                        Some(parent.start_position().row + 1),
                    );
                }
                "struct_item" | "enum_item" | "trait_item" => {
                    let name_node = parent.child_by_field_name("name");
                    return (
                        name_node.map(|n| get_node_text(&n, source).to_string()),
                        Some(parent.kind().to_string()),
                        Some(parent.start_position().row + 1),
                    );
                }
                _ => {}
            }
            curr = parent.parent();
        }
        (None, None, None)
    }

    fn get_class_context_rust(&self, node: &Node, source: &[u8]) -> Option<String> {
        let mut curr = node.parent();
        while let Some(parent) = curr {
            match parent.kind() {
                "impl_item" => {
                    for i in 0..parent.child_count() {
                        if let Some(child) = parent.child(i) {
                            if child.kind() == "type_identifier" {
                                return Some(get_node_text(&child, source).to_string());
                            }
                        }
                    }
                    return None;
                }
                "struct_item" | "enum_item" | "trait_item" => {
                    return parent
                        .child_by_field_name("name")
                        .map(|n| get_node_text(&n, source).to_string());
                }
                _ => {}
            }
            curr = parent.parent();
        }
        None
    }

    fn parse_rust_params(&self, params_node: &Node, source: &[u8]) -> Vec<String> {
        let mut args = Vec::new();
        for i in 0..params_node.child_count() {
            let child = match params_node.child(i) {
                Some(c) => c,
                None => continue,
            };
            match child.kind() {
                "parameter" => {
                    let pattern = child.child_by_field_name("pattern");
                    if let Some(p) = pattern {
                        let name = get_node_text(&p, source).to_string();
                        let type_node = child.child_by_field_name("type");
                        if let Some(t) = type_node {
                            args.push(format!("{}: {}", name, get_node_text(&t, source)));
                        } else {
                            args.push(name);
                        }
                    }
                }
                "self_parameter" => {
                    args.push(get_node_text(&child, source).to_string());
                }
                _ => {}
            }
        }
        args
    }
}

impl LanguageExtractor for RustExtractor {
    fn language(&self) -> Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "rust"
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
                Some(p) if p.kind() == "function_item" => p,
                _ => continue,
            };

            let node_id = (func_node.start_byte(), func_node.end_byte());
            if seen_nodes.contains(&node_id) {
                continue;
            }
            seen_nodes.insert(node_id);

            let name = get_node_text(&node, source).to_string();
            let params_node = func_node.child_by_field_name("parameters");
            let args = match params_node {
                Some(ref pn) => self.parse_rust_params(pn, source),
                None => Vec::new(),
            };

            let complexity = self.calculate_complexity(&func_node);
            let (context, context_type, _) = self.get_parent_context_rust(&func_node, source);
            let class_context = self.get_class_context_rust(&func_node, source);

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

        // Structs
        for (node, capture_name) in self.execute_query(QUERY_STRUCTS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let struct_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };
            let name = get_node_text(&node, source).to_string();
            let (context, _, _) = self.get_parent_context_rust(&struct_node, source);

            let mut class = ClassData {
                name,
                line_number: struct_node.start_position().row + 1,
                end_line: struct_node.end_position().row + 1,
                bases: Vec::new(),
                context,
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
                Some(p) => p,
                None => continue,
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

        // Traits
        for (node, capture_name) in self.execute_query(QUERY_TRAITS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let trait_node = match node.parent() {
                Some(p) => p,
                None => continue,
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

            let full_text = get_node_text(&node, source).to_string();

            // Extract alias if present: `use foo::bar as baz;`
            let (name, alias) = if let Some(as_pos) = full_text.find(" as ") {
                let after_as = full_text[as_pos + 4..].trim().trim_end_matches(';');
                (after_as.to_string(), Some(after_as.to_string()))
            } else {
                // Extract the last component of the path
                let cleaned = full_text
                    .trim_start_matches("use ")
                    .trim_end_matches(';')
                    .trim();
                let last_part = cleaned.rsplit("::").next().unwrap_or(cleaned);
                if last_part == "*" {
                    ("*".to_string(), None)
                } else {
                    // Handle curly brace groups: use std::{io, fs};
                    if last_part.contains('{') {
                        (cleaned.to_string(), None)
                    } else {
                        (last_part.to_string(), None)
                    }
                }
            };

            imports.push(ImportData {
                name,
                full_import_name: full_text,
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

        for (node, capture_name) in self.execute_query(QUERY_CALLS, root, source) {
            match capture_name.as_str() {
                "name" => {
                    // Walk up to find call_expression
                    let mut call_node = node.parent();
                    while let Some(cn) = call_node {
                        if cn.kind() == "call_expression" {
                            break;
                        }
                        call_node = cn.parent();
                    }
                    let call_node = match call_node {
                        Some(cn) if cn.kind() == "call_expression" => cn,
                        _ => continue,
                    };

                    let call_name = get_node_text(&node, source).to_string();
                    let full_name = call_node
                        .child_by_field_name("function")
                        .map(|f| get_node_text(&f, source).to_string())
                        .unwrap_or_else(|| call_name.clone());

                    // Extract arguments
                    let mut args = Vec::new();
                    if let Some(args_node) = call_node.child_by_field_name("arguments") {
                        for i in 0..args_node.child_count() {
                            if let Some(arg) = args_node.child(i) {
                                let text = get_node_text(&arg, source);
                                if !text.is_empty()
                                    && text != "("
                                    && text != ")"
                                    && text != ","
                                {
                                    args.push(text.to_string());
                                }
                            }
                        }
                    }

                    let (context_name, context_type, context_line) =
                        self.get_parent_context_rust(&node, source);
                    let class_context = self.get_class_context_rust(&node, source);

                    calls.push(CallData {
                        name: call_name,
                        full_name,
                        line_number: node.start_position().row + 1,
                        args,
                        inferred_obj_type: None,
                        context: (context_name, context_type, context_line),
                        class_context: (class_context, None),
                        lang: self.lang_name().to_string(),
                        is_dependency: false,
                        is_indirect_call: false,
                    });
                }
                "macro_name" => {
                    let macro_name = get_node_text(&node, source).to_string();
                    let macro_node = match node.parent() {
                        Some(p) => p,
                        None => continue,
                    };

                    let (context_name, context_type, context_line) =
                        self.get_parent_context_rust(&node, source);
                    let class_context = self.get_class_context_rust(&node, source);

                    calls.push(CallData {
                        name: format!("{}!", macro_name),
                        full_name: get_node_text(&macro_node, source).to_string(),
                        line_number: node.start_position().row + 1,
                        args: Vec::new(),
                        inferred_obj_type: None,
                        context: (context_name, context_type, context_line),
                        class_context: (class_context, None),
                        lang: self.lang_name().to_string(),
                        is_dependency: false,
                        is_indirect_call: false,
                    });
                }
                _ => continue,
            }
        }

        calls
    }

    fn find_variables(&self, root: &Node, source: &[u8]) -> Vec<VariableData> {
        let mut variables = Vec::new();

        for (node, capture_name) in self.execute_query(QUERY_VARIABLES, root, source) {
            if capture_name != "name" {
                continue;
            }

            let let_node = match node.parent() {
                Some(p) if p.kind() == "let_declaration" => p,
                _ => continue,
            };

            let name = get_node_text(&node, source).to_string();

            let value = let_node
                .child_by_field_name("value")
                .map(|v| get_node_text(&v, source).to_string());

            let type_annotation = let_node
                .child_by_field_name("type")
                .map(|t| get_node_text(&t, source).to_string());

            let (context, _, _) = self.get_parent_context_rust(&node, source);
            let class_context = self.get_class_context_rust(&node, source);

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
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn greet(name: &str) {
    println!("Hello {}", name);
}
"#;
        let (tree, source) = parse_source(code);
        let ext = RustExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 2);
        assert!(funcs.iter().any(|f| f.name == "add"));
        assert!(funcs.iter().any(|f| f.name == "greet"));
        let add = funcs.iter().find(|f| f.name == "add").unwrap();
        assert_eq!(add.args.len(), 2);
    }

    #[test]
    fn test_find_structs_enums_traits() {
        let code = r#"
struct Point {
    x: f64,
    y: f64,
}

enum Direction {
    North,
    South,
    East,
    West,
}

trait Drawable {
    fn draw(&self);
}
"#;
        let (tree, source) = parse_source(code);
        let ext = RustExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.iter().any(|c| c.name == "Point"));
        assert!(classes.iter().any(|c| c.name == "Direction"));
        assert!(classes.iter().any(|c| c.name == "Drawable"));
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
use std::collections::HashMap;
use std::io::{self, Read};
"#;
        let (tree, source) = parse_source(code);
        let ext = RustExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 2);
        assert!(imports.iter().any(|i| i.name == "HashMap"));
    }

    #[test]
    fn test_find_calls() {
        let code = r#"
fn main() {
    let x = add(1, 2);
    println!("result: {}", x);
    vec.push(42);
}
"#;
        let (tree, source) = parse_source(code);
        let ext = RustExtractor;
        let calls = ext.find_calls(&tree.root_node(), &source);
        assert!(calls.iter().any(|c| c.name == "add"));
        assert!(calls.iter().any(|c| c.name == "println!"));
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
fn example() {
    let x = 10;
    let name: String = String::from("hello");
}
"#;
        let (tree, source) = parse_source(code);
        let ext = RustExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert!(vars.len() >= 2);
        assert!(vars.iter().any(|v| v.name == "x"));
        assert!(vars.iter().any(|v| v.name == "name"));
    }

    #[test]
    fn test_complexity() {
        let code = r#"
fn complex(x: i32) -> i32 {
    if x > 0 {
        for i in 0..x {
            if i % 2 == 0 {
                while x > 0 {
                    break;
                }
            }
        }
    }
    match x {
        0 => 0,
        1 => 1,
        _ => x,
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = RustExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 1);
        // base 1 + if + for + if + while + match + 3 match_arms = high complexity
        assert!(funcs[0].cyclomatic_complexity >= 5);
    }
}
