use std::collections::HashSet;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, get_parent_context, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    "if_statement",
    "for_statement",
    "while_statement",
    "do_statement",
    "switch_statement",
    "case_statement",
    "conditional_expression",
];

const QUERY_FUNCTIONS: &str = r#"
    (function_definition
        declarator: (function_declarator
            declarator: (identifier) @name
        )
    ) @function_node

    (function_definition
        declarator: (function_declarator
            declarator: (pointer_declarator
                declarator: (identifier) @name
            )
        )
    ) @function_node
"#;

const QUERY_STRUCTS: &str = r#"
    (struct_specifier
        name: (type_identifier) @name
    ) @struct
"#;

const QUERY_UNIONS: &str = r#"
    (union_specifier
        name: (type_identifier) @name
    ) @union
"#;

const QUERY_ENUMS: &str = r#"
    (enum_specifier
        name: (type_identifier) @name
    ) @enum
"#;

const QUERY_IMPORTS: &str = r#"
    (preproc_include
        path: [
            (string_literal) @path
            (system_lib_string) @path
        ]
    ) @import
"#;

const QUERY_CALLS: &str = r#"
    (call_expression
        function: (identifier) @name
    )
"#;

const QUERY_VARIABLES: &str = r#"
    (declaration
        declarator: (init_declarator
            declarator: (identifier) @name
        )
    )

    (declaration
        declarator: (init_declarator
            declarator: (pointer_declarator
                declarator: (identifier) @name
            )
        )
    )

    (declaration
        declarator: (identifier) @name
    )

    (declaration
        declarator: (pointer_declarator
            declarator: (identifier) @name
        )
    )
"#;

const QUERY_MACROS: &str = r#"
    (preproc_def
        name: (identifier) @name
    ) @macro
"#;

const QUERY_PRE_SCAN: &str = r#"
    (function_definition
        declarator: (function_declarator
            declarator: (identifier) @name
        )
    )
    (struct_specifier name: (type_identifier) @name)
    (union_specifier name: (type_identifier) @name)
    (enum_specifier name: (type_identifier) @name)
    (preproc_def name: (identifier) @name)
"#;

pub struct CExtractor;

impl CExtractor {
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

    fn get_parent_context_c(
        &self,
        node: &Node,
        source: &[u8],
    ) -> (Option<String>, Option<String>, Option<usize>) {
        let mut curr = node.parent();
        while let Some(parent) = curr {
            match parent.kind() {
                "function_definition" => {
                    let mut decl = parent.child_by_field_name("declarator");
                    while let Some(d) = decl {
                        match d.kind() {
                            "identifier" => {
                                return (
                                    Some(get_node_text(&d, source).to_string()),
                                    Some(parent.kind().to_string()),
                                    Some(d.start_position().row + 1),
                                );
                            }
                            _ => {
                                decl = d.child_by_field_name("declarator");
                            }
                        }
                    }
                    return (
                        None,
                        Some(parent.kind().to_string()),
                        Some(parent.start_position().row + 1),
                    );
                }
                "struct_specifier" | "union_specifier" | "enum_specifier" => {
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

    fn extract_function_params(&self, func_node: &Node, source: &[u8]) -> Vec<String> {
        let mut params = Vec::new();
        let declarator_node = match func_node.child_by_field_name("declarator") {
            Some(d) => d,
            None => return params,
        };
        let parameters_node = match declarator_node.child_by_field_name("parameters") {
            Some(p) if p.kind() == "parameter_list" => p,
            _ => return params,
        };

        for i in 0..parameters_node.child_count() {
            let param = match parameters_node.child(i) {
                Some(p) if p.kind() == "parameter_declaration" => p,
                _ => continue,
            };

            let mut param_decl = param.child_by_field_name("declarator");
            while let Some(pd) = param_decl {
                match pd.kind() {
                    "identifier" => {
                        params.push(get_node_text(&pd, source).to_string());
                        break;
                    }
                    _ => {
                        param_decl = pd.child_by_field_name("declarator");
                        if param_decl.is_none() {
                            break;
                        }
                    }
                }
            }
        }
        params
    }
}

impl LanguageExtractor for CExtractor {
    fn language(&self) -> Language {
        tree_sitter_c::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "c"
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

            // Walk up to find the function_definition node
            let mut func_node_opt = node.parent();
            while let Some(fn_node) = func_node_opt {
                if fn_node.kind() == "function_definition" {
                    break;
                }
                func_node_opt = fn_node.parent();
            }
            let func_node = match func_node_opt {
                Some(fn_node) if fn_node.kind() == "function_definition" => fn_node,
                _ => continue,
            };

            let node_id = (func_node.start_byte(), func_node.end_byte());
            if seen_nodes.contains(&node_id) {
                continue;
            }
            seen_nodes.insert(node_id);

            let name = get_node_text(&node, source).to_string();
            let params = self.extract_function_params(&func_node, source);
            let complexity = self.calculate_complexity(&func_node);
            let (context, context_type, _) = self.get_parent_context_c(&func_node, source);

            let mut func = FunctionData {
                name,
                line_number: node.start_position().row + 1,
                end_line: func_node.end_position().row + 1,
                args: params,
                cyclomatic_complexity: complexity,
                context,
                context_type,
                class_context: None,
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
            let (context, _, _) = self.get_parent_context_c(&struct_node, source);

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

        // Unions
        for (node, capture_name) in self.execute_query(QUERY_UNIONS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let union_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };
            let name = get_node_text(&node, source).to_string();

            let mut class = ClassData {
                name,
                line_number: union_node.start_position().row + 1,
                end_line: union_node.end_position().row + 1,
                bases: Vec::new(),
                context: None,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&union_node, source).to_string());
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

        // Macros (as ClassData for indexing)
        for (node, capture_name) in self.execute_query(QUERY_MACROS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let macro_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };
            let name = get_node_text(&node, source).to_string();

            let mut class = ClassData {
                name,
                line_number: macro_node.start_position().row + 1,
                end_line: macro_node.end_position().row + 1,
                bases: Vec::new(),
                context: None,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&macro_node, source).to_string());
            }

            classes.push(class);
        }

        classes
    }

    fn find_imports(&self, root: &Node, source: &[u8]) -> Vec<ImportData> {
        let mut imports = Vec::new();

        for (node, capture_name) in self.execute_query(QUERY_IMPORTS, root, source) {
            if capture_name != "path" {
                continue;
            }

            let raw_path = get_node_text(&node, source);
            let path = raw_path
                .trim_matches('"')
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();

            imports.push(ImportData {
                name: path.clone(),
                full_import_name: path,
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

            let call_node = match node.parent() {
                Some(p) if p.kind() == "call_expression" => p,
                _ => continue,
            };

            let call_name = get_node_text(&node, source).to_string();

            // Extract arguments
            let mut args = Vec::new();
            if let Some(args_node) = call_node.child_by_field_name("arguments") {
                for i in 0..args_node.child_count() {
                    if let Some(arg) = args_node.child(i) {
                        let text = get_node_text(&arg, source);
                        if !text.is_empty() && text != "(" && text != ")" && text != "," {
                            args.push(text.to_string());
                        }
                    }
                }
            }

            let (context_name, context_type, context_line) =
                self.get_parent_context_c(&node, source);

            calls.push(CallData {
                name: call_name.clone(),
                full_name: call_name,
                line_number: node.start_position().row + 1,
                args,
                inferred_obj_type: None,
                context: (context_name, context_type, context_line),
                class_context: (None, None),
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

            // Walk up to the declaration node to get type
            let mut decl_node = node.parent();
            while let Some(d) = decl_node {
                if d.kind() == "declaration" {
                    break;
                }
                decl_node = d.parent();
            }

            let type_annotation = decl_node.and_then(|d| {
                d.child_by_field_name("type")
                    .map(|t| get_node_text(&t, source).to_string())
            });

            let value = node.parent().and_then(|p| {
                if p.kind() == "init_declarator" {
                    p.child_by_field_name("value")
                        .map(|v| get_node_text(&v, source).to_string())
                } else {
                    None
                }
            });

            let (context, _, _) = self.get_parent_context_c(&node, source);
            let class_context = {
                let (cc, _, _) = get_parent_context(
                    &node,
                    source,
                    &["struct_specifier", "union_specifier", "enum_specifier"],
                );
                cc
            };

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
        let lang: Language = tree_sitter_c::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
int add(int a, int b) {
    return a + b;
}

void greet(const char* name) {
    printf("Hello %s\n", name);
}
"#;
        let (tree, source) = parse_source(code);
        let ext = CExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 2);
        assert!(funcs.iter().any(|f| f.name == "add"));
        assert!(funcs.iter().any(|f| f.name == "greet"));
    }

    #[test]
    fn test_find_structs_and_enums() {
        let code = r#"
struct Point {
    int x;
    int y;
};

enum Color {
    RED,
    GREEN,
    BLUE
};

union Data {
    int i;
    float f;
};

#define MAX_SIZE 100
"#;
        let (tree, source) = parse_source(code);
        let ext = CExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.iter().any(|c| c.name == "Point"));
        assert!(classes.iter().any(|c| c.name == "Color"));
        assert!(classes.iter().any(|c| c.name == "Data"));
        assert!(classes.iter().any(|c| c.name == "MAX_SIZE"));
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
#include <stdio.h>
#include "mylib.h"
"#;
        let (tree, source) = parse_source(code);
        let ext = CExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "stdio.h");
        assert_eq!(imports[1].name, "mylib.h");
    }

    #[test]
    fn test_find_calls() {
        let code = r#"
void test() {
    printf("hello");
    malloc(100);
}
"#;
        let (tree, source) = parse_source(code);
        let ext = CExtractor;
        let calls = ext.find_calls(&tree.root_node(), &source);
        assert!(calls.len() >= 2);
        assert!(calls.iter().any(|c| c.name == "printf"));
        assert!(calls.iter().any(|c| c.name == "malloc"));
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
int x = 10;
float pi = 3.14;
"#;
        let (tree, source) = parse_source(code);
        let ext = CExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert!(vars.len() >= 2);
        assert!(vars.iter().any(|v| v.name == "x"));
        assert!(vars.iter().any(|v| v.name == "pi"));
    }

    #[test]
    fn test_complexity() {
        let code = r#"
int complex(int x) {
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
"#;
        let (tree, source) = parse_source(code);
        let ext = CExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 1);
        // base 1 + if + for + if + while = 5
        assert!(funcs[0].cyclomatic_complexity >= 4);
    }
}
