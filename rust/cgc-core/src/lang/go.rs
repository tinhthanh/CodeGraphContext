use std::collections::{HashMap, HashSet};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, get_parent_context, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    "if_statement",
    "for_statement",
    "expression_switch_statement",
    "type_switch_statement",
    "select_statement",
    "expression_case",
    "default_case",
    "communication_case",
];

const QUERY_FUNCTIONS: &str = r#"
    (function_declaration
        name: (identifier) @name
        parameters: (parameter_list) @params
    ) @function_node

    (method_declaration
        receiver: (parameter_list) @receiver
        name: (field_identifier) @name
        parameters: (parameter_list) @params
    ) @function_node
"#;

const QUERY_STRUCTS: &str = r#"
    (type_declaration
        (type_spec
            name: (type_identifier) @name
            type: (struct_type) @struct_body
        )
    ) @struct_node
"#;

const QUERY_INTERFACES: &str = r#"
    (type_declaration
        (type_spec
            name: (type_identifier) @name
            type: (interface_type) @interface_body
        )
    ) @interface_node
"#;

const QUERY_IMPORTS: &str = r#"
    (import_spec
        path: (interpreted_string_literal) @path
    )
"#;

const QUERY_CALLS: &str = r#"
    (call_expression
        function: (identifier) @name
    )
    (call_expression
        function: (selector_expression
            field: (field_identifier) @name
        )
    )
"#;

const QUERY_VARIABLES: &str = r#"
    (var_declaration
        (var_spec
            name: (identifier) @name
        )
    )
    (short_var_declaration
        left: (expression_list
            (identifier) @name
        )
    )
"#;

const QUERY_PRE_SCAN: &str = r#"
    (function_declaration name: (identifier) @name)
    (method_declaration name: (field_identifier) @name)
    (type_declaration (type_spec name: (type_identifier) @name))
"#;

pub struct GoExtractor;

impl GoExtractor {
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

    fn find_function_node_for<'a>(&self, node: &Node<'a>) -> Option<Node<'a>> {
        let mut current = node.parent();
        while let Some(n) = current {
            if n.kind() == "function_declaration" || n.kind() == "method_declaration" {
                return Some(n);
            }
            current = n.parent();
        }
        None
    }

    fn find_type_declaration_for<'a>(&self, node: &Node<'a>) -> Option<Node<'a>> {
        let mut current = node.parent();
        while let Some(n) = current {
            if n.kind() == "type_declaration" {
                return Some(n);
            }
            current = n.parent();
        }
        None
    }

    fn extract_parameters(&self, params_node: &Node, source: &[u8]) -> Vec<String> {
        let mut params = Vec::new();
        if params_node.kind() != "parameter_list" {
            return params;
        }
        for i in 0..params_node.child_count() {
            let child = match params_node.child(i) {
                Some(c) => c,
                None => continue,
            };
            if child.kind() == "parameter_declaration" {
                let type_node = child.child_by_field_name("type");
                let type_id = type_node.as_ref().map(|n| n.id());
                for j in 0..child.child_count() {
                    if let Some(gc) = child.child(j) {
                        if gc.kind() == "identifier" {
                            if Some(gc.id()) != type_id {
                                params.push(get_node_text(&gc, source).to_string());
                            }
                        }
                    }
                }
            } else if child.kind() == "variadic_parameter_declaration" {
                let name_node = child.child_by_field_name("name");
                if let Some(n) = name_node {
                    params.push(format!("...{}", get_node_text(&n, source)));
                }
            }
        }
        params
    }

    fn extract_receiver(&self, receiver_node: &Node, source: &[u8]) -> Option<String> {
        if receiver_node.kind() != "parameter_list" {
            return None;
        }
        // First named child should be the parameter_declaration
        for i in 0..receiver_node.child_count() {
            if let Some(child) = receiver_node.child(i) {
                if child.kind() == "parameter_declaration" {
                    let type_node = child.child_by_field_name("type");
                    if let Some(tn) = type_node {
                        let type_text = get_node_text(&tn, source);
                        return Some(type_text.trim_start_matches('*').to_string());
                    }
                }
            }
        }
        None
    }

    fn get_docstring(&self, func_node: &Node, source: &[u8]) -> Option<String> {
        let mut prev = func_node.prev_sibling();
        while let Some(sib) = prev {
            match sib.kind() {
                "comment" => {
                    let text = get_node_text(&sib, source);
                    if text.starts_with("//") {
                        return Some(text.trim().to_string());
                    }
                }
                _ => break,
            }
            prev = sib.prev_sibling();
        }
        None
    }

    /// Calculate complexity, also counting binary_expression with && or ||.
    fn calculate_go_complexity(&self, node: &Node, source: &[u8]) -> usize {
        let mut count = 1usize;
        self.walk_go_complexity(node, source, &mut count);
        count
    }

    fn walk_go_complexity(&self, node: &Node, source: &[u8], count: &mut usize) {
        let kind = node.kind();
        if COMPLEXITY_TYPES.contains(&kind) {
            *count += 1;
        } else if kind == "binary_expression" {
            let text = get_node_text(node, source);
            if text.contains("&&") || text.contains("||") {
                *count += 1;
            }
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.walk_go_complexity(&child, source, count);
            }
        }
    }
}

impl LanguageExtractor for GoExtractor {
    fn language(&self) -> Language {
        tree_sitter_go::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "go"
    }

    fn complexity_node_types(&self) -> &[&str] {
        COMPLEXITY_TYPES
    }

    fn calculate_complexity(&self, node: &Node) -> usize {
        // We need source bytes for binary_expression check, but the trait
        // signature doesn't pass source. For the trait's default we fall back
        // to the base implementation. The actual complexity is calculated via
        // calculate_go_complexity when we have source available.
        let types = self.complexity_node_types();
        let mut count = 1usize;
        super::walk_complexity(node, types, &mut count);
        count
    }

    fn find_functions(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<FunctionData> {
        let mut functions = Vec::new();

        // Group captures by function node id
        let mut captures_by_func: HashMap<usize, (Node, Option<String>, Option<Node>, Option<Node>)> =
            HashMap::new();

        let captures = self.execute_query(QUERY_FUNCTIONS, root, source);
        for (node, capture_name) in &captures {
            match capture_name.as_str() {
                "function_node" => {
                    let fid = node.id();
                    captures_by_func
                        .entry(fid)
                        .or_insert((*node, None, None, None));
                }
                "name" => {
                    if let Some(func_node) = self.find_function_node_for(node) {
                        let fid = func_node.id();
                        let entry = captures_by_func
                            .entry(fid)
                            .or_insert((func_node, None, None, None));
                        entry.1 = Some(get_node_text(node, source).to_string());
                    }
                }
                "params" => {
                    if let Some(func_node) = self.find_function_node_for(node) {
                        let fid = func_node.id();
                        let entry = captures_by_func
                            .entry(fid)
                            .or_insert((func_node, None, None, None));
                        entry.2 = Some(*node);
                    }
                }
                "receiver" => {
                    if let Some(parent) = node.parent() {
                        if parent.kind() == "method_declaration" {
                            let fid = parent.id();
                            let entry = captures_by_func
                                .entry(fid)
                                .or_insert((parent, None, None, None));
                            entry.3 = Some(*node);
                        }
                    }
                }
                _ => {}
            }
        }

        let fc_types = &[
            "function_declaration",
            "method_declaration",
            "type_declaration",
        ];

        for (_fid, (func_node, name_opt, params_opt, receiver_opt)) in &captures_by_func {
            let name = match name_opt {
                Some(n) => n.clone(),
                None => continue,
            };

            let args = match params_opt {
                Some(pn) => self.extract_parameters(pn, source),
                None => Vec::new(),
            };

            let receiver_type = receiver_opt
                .as_ref()
                .and_then(|rn| self.extract_receiver(rn, source));

            let (context, context_type, _) = get_parent_context(func_node, source, fc_types);
            let class_context = receiver_type.or_else(|| {
                if context_type.as_deref() == Some("type_declaration") {
                    context.clone()
                } else {
                    None
                }
            });

            let complexity = self.calculate_go_complexity(func_node, source);

            let mut func = FunctionData {
                name,
                line_number: func_node.start_position().row + 1,
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
                func.source = Some(get_node_text(func_node, source).to_string());
                func.docstring = self.get_docstring(func_node, source);
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
            let type_decl = match self.find_type_declaration_for(&node) {
                Some(td) => td,
                None => continue,
            };
            let name = get_node_text(&node, source).to_string();

            let mut class = ClassData {
                name,
                line_number: type_decl.start_position().row + 1,
                end_line: type_decl.end_position().row + 1,
                bases: Vec::new(),
                context: None,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&type_decl, source).to_string());
                class.docstring = self.get_docstring(&type_decl, source);
            }

            classes.push(class);
        }

        // Interfaces
        for (node, capture_name) in self.execute_query(QUERY_INTERFACES, root, source) {
            if capture_name != "name" {
                continue;
            }
            let type_decl = match self.find_type_declaration_for(&node) {
                Some(td) => td,
                None => continue,
            };
            let name = get_node_text(&node, source).to_string();

            let mut class = ClassData {
                name,
                line_number: type_decl.start_position().row + 1,
                end_line: type_decl.end_position().row + 1,
                bases: Vec::new(),
                context: None,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&type_decl, source).to_string());
                class.docstring = self.get_docstring(&type_decl, source);
            }

            classes.push(class);
        }

        classes
    }

    fn find_imports(&self, root: &Node, source: &[u8]) -> Vec<ImportData> {
        let mut imports = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_IMPORTS, root, source) {
            if capture_name != "path" {
                continue;
            }

            let path_text = get_node_text(&node, source)
                .trim_matches('"')
                .to_string();

            if seen.contains(&path_text) {
                continue;
            }
            seen.insert(path_text.clone());

            let package_name = path_text
                .rsplit('/')
                .next()
                .unwrap_or(&path_text)
                .to_string();

            // Check for alias on the import_spec parent
            let alias = node.parent().and_then(|import_spec| {
                if import_spec.kind() == "import_spec" {
                    import_spec
                        .child_by_field_name("name")
                        .map(|n| get_node_text(&n, source).to_string())
                } else {
                    None
                }
            });

            imports.push(ImportData {
                name: package_name,
                full_import_name: path_text,
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
        let mut seen_calls: HashSet<String> = HashSet::new();
        let fc_types = &[
            "function_declaration",
            "method_declaration",
            "type_declaration",
        ];

        for (node, capture_name) in self.execute_query(QUERY_CALLS, root, source) {
            if capture_name != "name" {
                continue;
            }

            // Navigate up to call_expression
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

            let name = get_node_text(&node, source).to_string();
            let line_number = node.start_position().row + 1;

            let call_key = format!("{}_{}", name, line_number);
            if seen_calls.contains(&call_key) {
                continue;
            }
            seen_calls.insert(call_key);

            let full_name = call_node
                .child_by_field_name("function")
                .map(|n| get_node_text(&n, source).to_string())
                .unwrap_or_else(|| name.clone());

            let context = get_parent_context(&node, source, fc_types);

            calls.push(CallData {
                name,
                full_name,
                line_number,
                args: Vec::new(),
                inferred_obj_type: None,
                context,
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
        let fc_types = &[
            "function_declaration",
            "method_declaration",
            "type_declaration",
        ];

        for (node, capture_name) in self.execute_query(QUERY_VARIABLES, root, source) {
            if capture_name != "name" {
                continue;
            }

            let name = get_node_text(&node, source).to_string();
            let (context, _, _) = get_parent_context(&node, source, fc_types);

            variables.push(VariableData {
                name,
                line_number: node.start_position().row + 1,
                value: None,
                type_annotation: None,
                context,
                class_context: None,
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
        let lang: Language = tree_sitter_go::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
package main

func hello(name string, age int) {
    fmt.Println(name)
}

func add(a, b int) int {
    return a + b
}
"#;
        let (tree, source) = parse_source(code);
        let ext = GoExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 2);

        let hello = funcs.iter().find(|f| f.name == "hello").unwrap();
        assert_eq!(hello.args, vec!["name", "age"]);

        let add = funcs.iter().find(|f| f.name == "add").unwrap();
        assert_eq!(add.args, vec!["a", "b"]);
    }

    #[test]
    fn test_find_method_with_receiver() {
        let code = r#"
package main

type Server struct {
    port int
}

func (s *Server) Start() {
    fmt.Println("starting")
}
"#;
        let (tree, source) = parse_source(code);
        let ext = GoExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        let start = funcs.iter().find(|f| f.name == "Start").unwrap();
        assert_eq!(start.class_context.as_deref(), Some("Server"));
    }

    #[test]
    fn test_find_structs_and_interfaces() {
        let code = r#"
package main

type Animal struct {
    Name string
}

type Speaker interface {
    Speak() string
}
"#;
        let (tree, source) = parse_source(code);
        let ext = GoExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert_eq!(classes.len(), 2);
        assert!(classes.iter().any(|c| c.name == "Animal"));
        assert!(classes.iter().any(|c| c.name == "Speaker"));
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
package main

import (
    "fmt"
    "net/http"
    log "github.com/sirupsen/logrus"
)
"#;
        let (tree, source) = parse_source(code);
        let ext = GoExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 2);

        let fmt_import = imports.iter().find(|i| i.name == "fmt").unwrap();
        assert_eq!(fmt_import.full_import_name, "fmt");

        let http_import = imports.iter().find(|i| i.name == "http").unwrap();
        assert_eq!(http_import.full_import_name, "net/http");
    }

    #[test]
    fn test_find_calls() {
        let code = r#"
package main

func main() {
    fmt.Println("hello")
    doWork()
}
"#;
        let (tree, source) = parse_source(code);
        let ext = GoExtractor;
        let calls = ext.find_calls(&tree.root_node(), &source);
        assert!(calls.len() >= 2);
        assert!(calls.iter().any(|c| c.name == "Println"));
        assert!(calls.iter().any(|c| c.name == "doWork"));
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
package main

var globalVar int

func main() {
    x := 10
    y := "hello"
}
"#;
        let (tree, source) = parse_source(code);
        let ext = GoExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert!(vars.len() >= 2);
        assert!(vars.iter().any(|v| v.name == "globalVar"));
        assert!(vars.iter().any(|v| v.name == "x"));
    }

    #[test]
    fn test_pre_scan() {
        let code = r#"
package main

type MyStruct struct {}
func myFunc() {}
"#;
        let (tree, source) = parse_source(code);
        let ext = GoExtractor;
        let names = ext.pre_scan_definitions(&tree.root_node(), &source);
        assert!(names.contains(&"MyStruct".to_string()));
        assert!(names.contains(&"myFunc".to_string()));
    }
}
