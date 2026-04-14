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
    "switch_expression",
    "catch_clause",
    "conditional_expression",
];

const QUERY_FUNCTIONS: &str = r#"
    (method_declaration
        name: (identifier) @name
        parameters: (formal_parameters) @params
    ) @function_node

    (constructor_declaration
        name: (identifier) @name
        parameters: (formal_parameters) @params
    ) @function_node
"#;

const QUERY_CLASSES: &str = r#"
    [
        (class_declaration name: (identifier) @name)
        (interface_declaration name: (identifier) @name)
        (enum_declaration name: (identifier) @name)
        (annotation_type_declaration name: (identifier) @name)
    ] @class
"#;

const QUERY_IMPORTS: &str = r#"
    (import_declaration) @import
"#;

const QUERY_CALLS: &str = r#"
    (method_invocation
        name: (identifier) @name
    ) @call_node

    (object_creation_expression
        type: [
            (type_identifier)
            (scoped_type_identifier)
            (generic_type)
        ] @name
    ) @call_node
"#;

const QUERY_VARIABLES: &str = r#"
    (local_variable_declaration
        type: (_) @type
        declarator: (variable_declarator
            name: (identifier) @name
        )
    ) @variable

    (field_declaration
        type: (_) @type
        declarator: (variable_declarator
            name: (identifier) @name
        )
    ) @variable
"#;

const QUERY_PRE_SCAN: &str = r#"
    (class_declaration name: (identifier) @name)
    (interface_declaration name: (identifier) @name)
"#;

pub struct JavaExtractor;

impl JavaExtractor {
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

    fn get_parent_context_java(
        &self,
        node: &Node,
        source: &[u8],
    ) -> (Option<String>, Option<String>, Option<usize>) {
        let types = &[
            "method_declaration",
            "constructor_declaration",
            "class_declaration",
            "interface_declaration",
            "enum_declaration",
            "annotation_type_declaration",
        ];
        get_parent_context(node, source, types)
    }

    fn get_class_context(
        &self,
        node: &Node,
        source: &[u8],
    ) -> (Option<String>, Option<String>) {
        let types = &[
            "class_declaration",
            "interface_declaration",
            "enum_declaration",
            "annotation_type_declaration",
        ];
        let (name, kind, _) = get_parent_context(node, source, types);
        (name, kind)
    }

    /// Extract parameter names from a formal_parameters text like "(int x, String name)"
    fn extract_parameter_names(&self, params_text: &str) -> Vec<String> {
        let mut params = Vec::new();
        let content = params_text.trim().trim_start_matches('(').trim_end_matches(')');
        if content.is_empty() {
            return params;
        }
        for param in content.split(',') {
            let param = param.trim();
            if param.is_empty() {
                continue;
            }
            let parts: Vec<&str> = param.split_whitespace().collect();
            if parts.len() >= 2 {
                params.push(parts.last().unwrap().to_string());
            }
        }
        params
    }
}

impl LanguageExtractor for JavaExtractor {
    fn language(&self) -> Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "java"
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
            if capture_name != "function_node" {
                continue;
            }

            let node_id = (node.start_byte(), node.end_byte());
            if seen_nodes.contains(&node_id) {
                continue;
            }
            seen_nodes.insert(node_id);

            let name_node = match node.child_by_field_name("name") {
                Some(n) => n,
                None => continue,
            };
            let func_name = get_node_text(&name_node, source).to_string();

            let params = node
                .child_by_field_name("parameters")
                .map(|pn| {
                    let params_text = get_node_text(&pn, source);
                    self.extract_parameter_names(params_text)
                })
                .unwrap_or_default();

            let (context, context_type, _) = self.get_parent_context_java(&node, source);
            let (class_name, class_type) = self.get_class_context(&node, source);
            let class_context = if class_type.is_some() {
                class_name
            } else {
                None
            };

            let complexity = self.calculate_complexity(&node);

            let mut func = FunctionData {
                name: func_name,
                line_number: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                args: params,
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

        functions
    }

    fn find_classes(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<ClassData> {
        let mut classes = Vec::new();
        let mut seen_nodes: HashSet<(usize, usize)> = HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_CLASSES, root, source) {
            if capture_name != "class" {
                continue;
            }

            let node_id = (node.start_byte(), node.end_byte());
            if seen_nodes.contains(&node_id) {
                continue;
            }
            seen_nodes.insert(node_id);

            let name_node = match node.child_by_field_name("name") {
                Some(n) => n,
                None => continue,
            };
            let class_name = get_node_text(&name_node, source).to_string();

            let mut bases = Vec::new();

            // Look for superclass (extends)
            if let Some(superclass_node) = node.child_by_field_name("superclass") {
                bases.push(get_node_text(&superclass_node, source).to_string());
            }

            // Look for interfaces (implements) - try field name first, then scan children
            let interfaces_node = node
                .child_by_field_name("interfaces")
                .or_else(|| {
                    (0..node.child_count())
                        .filter_map(|i| node.child(i))
                        .find(|c| c.kind() == "super_interfaces")
                });

            if let Some(ifaces) = interfaces_node {
                // Find type_list within
                let type_list = ifaces
                    .child_by_field_name("list")
                    .or_else(|| {
                        (0..ifaces.child_count())
                            .filter_map(|i| ifaces.child(i))
                            .find(|c| c.kind() == "type_list")
                    });

                if let Some(tl) = type_list {
                    for i in 0..tl.child_count() {
                        if let Some(child) = tl.child(i) {
                            if matches!(
                                child.kind(),
                                "type_identifier" | "generic_type" | "scoped_type_identifier"
                            ) {
                                bases.push(get_node_text(&child, source).to_string());
                            }
                        }
                    }
                } else {
                    // Fallback: scan children of interfaces node directly
                    for i in 0..ifaces.child_count() {
                        if let Some(child) = ifaces.child(i) {
                            if matches!(
                                child.kind(),
                                "type_identifier" | "generic_type" | "scoped_type_identifier"
                            ) {
                                bases.push(get_node_text(&child, source).to_string());
                            }
                        }
                    }
                }
            }

            let (context, _, _) = get_parent_context(
                &node,
                source,
                &["class_declaration", "interface_declaration"],
            );

            let mut class = ClassData {
                name: class_name,
                line_number: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                bases,
                context,
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
            if capture_name != "import" {
                continue;
            }

            let import_text = get_node_text(&node, source);

            // Parse: import [static] path;
            let trimmed = import_text.trim();
            let path = if let Some(rest) = trimmed.strip_prefix("import") {
                let rest = rest.trim();
                let rest = if let Some(r) = rest.strip_prefix("static") {
                    r.trim()
                } else {
                    rest
                };
                rest.trim_end_matches(';').trim().to_string()
            } else {
                continue;
            };

            if path.is_empty() {
                continue;
            }

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
        let mut seen_calls: HashSet<String> = HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_CALLS, root, source) {
            if capture_name != "name" {
                continue;
            }

            let call_name = get_node_text(&node, source).to_string();
            let line_number = node.start_position().row + 1;

            let call_key = format!("{}_{}", call_name, line_number);
            if seen_calls.contains(&call_key) {
                continue;
            }
            seen_calls.insert(call_key);

            // Navigate up to call_node
            let mut call_node = node.parent();
            while let Some(cn) = call_node {
                if cn.kind() == "method_invocation"
                    || cn.kind() == "object_creation_expression"
                {
                    break;
                }
                call_node = cn.parent();
            }
            let call_node = match call_node {
                Some(cn) => cn,
                None => continue,
            };

            // Extract args
            let mut args = Vec::new();
            let args_node = (0..call_node.child_count())
                .filter_map(|i| call_node.child(i))
                .find(|c| c.kind() == "argument_list");
            if let Some(an) = args_node {
                for i in 0..an.child_count() {
                    if let Some(arg) = an.child(i) {
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

            // Determine full_name and inferred_obj_type
            let mut full_name = call_name.clone();
            let mut inferred_obj_type = None;

            if call_node.kind() == "method_invocation" {
                if let Some(obj_node) = call_node.child_by_field_name("object") {
                    let obj_text = get_node_text(&obj_node, source);
                    full_name = format!("{}.{}", obj_text, call_name);
                    // Simple identifier resolution
                    let base_obj = obj_text.split('.').next().unwrap_or(obj_text);
                    if !base_obj.contains('(') {
                        inferred_obj_type = Some(base_obj.to_string());
                    }
                }
            } else if call_node.kind() == "object_creation_expression" {
                if let Some(type_node) = call_node.child_by_field_name("type") {
                    full_name = get_node_text(&type_node, source).to_string();
                }
            }

            let context = self.get_parent_context_java(&node, source);
            let (class_name, class_type) = self.get_class_context(&node, source);

            calls.push(CallData {
                name: call_name,
                full_name,
                line_number,
                args,
                inferred_obj_type,
                context,
                class_context: (class_name, class_type),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                is_indirect_call: false,
            });
        }

        calls
    }

    fn find_variables(&self, root: &Node, source: &[u8]) -> Vec<VariableData> {
        let mut variables = Vec::new();
        let mut seen_vars: HashSet<usize> = HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_VARIABLES, root, source) {
            if capture_name != "name" {
                continue;
            }

            let start_byte = node.start_byte();
            if seen_vars.contains(&start_byte) {
                continue;
            }
            seen_vars.insert(start_byte);

            // node is the identifier inside variable_declarator
            // Walk up: variable_declarator -> declaration
            let declaration = match node.parent().and_then(|p| p.parent()) {
                Some(d) => d,
                None => continue,
            };

            let type_annotation = declaration
                .child_by_field_name("type")
                .map(|tn| get_node_text(&tn, source).to_string());

            let var_name = get_node_text(&node, source).to_string();

            let (context, context_type, _) = self.get_parent_context_java(&node, source);
            let (class_name, class_type) = self.get_class_context(&node, source);
            let class_context = if class_type.is_some() {
                class_name
            } else {
                None
            };

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
        let lang: Language = tree_sitter_java::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
public class MyClass {
    public void hello(String name, int age) {
        System.out.println(name);
    }

    public int add(int a, int b) {
        return a + b;
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 2);

        let hello = funcs.iter().find(|f| f.name == "hello").unwrap();
        assert_eq!(hello.args, vec!["name", "age"]);

        let add = funcs.iter().find(|f| f.name == "add").unwrap();
        assert_eq!(add.args, vec!["a", "b"]);
    }

    #[test]
    fn test_find_classes() {
        let code = r#"
public class Animal {
    String name;
}

public class Dog extends Animal {
    public void bark() {}
}
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert_eq!(classes.len(), 2);
        assert_eq!(classes[0].name, "Animal");
        assert!(classes[0].bases.is_empty());
        assert_eq!(classes[1].name, "Dog");
        assert!(!classes[1].bases.is_empty());
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
import java.util.List;
import static java.lang.Math.PI;
import java.io.*;
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 2);
        assert_eq!(imports[0].name, "java.util.List");
        assert_eq!(imports[1].name, "java.lang.Math.PI");
    }

    #[test]
    fn test_find_calls() {
        let code = r#"
public class Main {
    public void run() {
        System.out.println("hello");
        List<String> items = new ArrayList<>();
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaExtractor;
        let calls = ext.find_calls(&tree.root_node(), &source);
        assert!(calls.len() >= 1);
        assert!(calls.iter().any(|c| c.name == "println"));
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
public class Main {
    private int count;
    public void run() {
        String name = "hello";
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert!(vars.len() >= 2);
        assert!(vars.iter().any(|v| v.name == "count"));
        assert!(vars.iter().any(|v| v.name == "name"));
    }

    #[test]
    fn test_pre_scan() {
        let code = r#"
public class MyService {
}

public interface MyInterface {
}
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaExtractor;
        let names = ext.pre_scan_definitions(&tree.root_node(), &source);
        assert!(names.contains(&"MyService".to_string()));
        assert!(names.contains(&"MyInterface".to_string()));
    }
}
