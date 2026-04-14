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
    "catch_clause",
];

const QUERY_FUNCTIONS: &str = r#"
    (function_definition
        declarator: (function_declarator
            declarator: [
                (identifier) @name
                (field_identifier) @name
                (qualified_identifier) @qualified_name
            ]
        )
    ) @function_node
"#;

const QUERY_CLASSES: &str = r#"
    (class_specifier
        name: (type_identifier) @name
    ) @class
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
        function: (identifier) @function_name
    )
    (call_expression
        function: (field_expression
            field: (field_identifier) @method_name
        )
    )
    (call_expression
        function: (qualified_identifier) @scoped_name
    )
"#;

const QUERY_VARIABLES: &str = r#"
    (declaration
        declarator: (init_declarator
            declarator: (identifier) @name))

    (declaration
        declarator: (init_declarator
            declarator: (pointer_declarator
                declarator: (identifier) @name)))

    (field_declaration
        declarator: [
            (field_identifier) @name
            (pointer_declarator declarator: (field_identifier) @name)
            (array_declarator declarator: (field_identifier) @name)
            (reference_declarator (field_identifier) @name)
        ]
    )
"#;

const QUERY_LAMBDA_ASSIGNMENTS: &str = r#"
    (declaration
        declarator: (init_declarator
            declarator: (identifier) @name
            value: (lambda_expression) @lambda_node))
"#;

const QUERY_STRUCTS: &str = r#"
    (struct_specifier
        name: (type_identifier) @name
        body: (field_declaration_list)? @body
    ) @struct
"#;

const QUERY_ENUMS: &str = r#"
    (enum_specifier
        name: (type_identifier) @name
    ) @enum
"#;

const QUERY_UNIONS: &str = r#"
    (union_specifier
        name: (type_identifier)? @name
    ) @union
"#;

const QUERY_MACROS: &str = r#"
    (preproc_def
        name: (identifier) @name
    ) @macro
"#;

const QUERY_PRE_SCAN: &str = r#"
    (class_specifier name: (type_identifier) @name)
    (struct_specifier name: (type_identifier) @name)
    (function_definition declarator: (function_declarator declarator: (identifier) @name))
    (function_definition declarator: (function_declarator declarator: (qualified_identifier) @qualified_name))
"#;

pub struct CppExtractor;

impl CppExtractor {
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

    fn get_parent_context_cpp(
        &self,
        node: &Node,
        source: &[u8],
    ) -> (Option<String>, Option<String>, Option<usize>) {
        let mut curr = node.parent();
        while let Some(parent) = curr {
            match parent.kind() {
                "function_definition" => {
                    // Traverse declarator to find name
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
                            "qualified_identifier" => {
                                let text = get_node_text(&d, source);
                                let name = if text.contains("::") {
                                    text.rsplit("::").next().unwrap_or(text)
                                } else {
                                    text
                                };
                                return (
                                    Some(name.to_string()),
                                    Some(parent.kind().to_string()),
                                    Some(d.start_position().row + 1),
                                );
                            }
                            "field_identifier" => {
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
                "class_specifier" => {
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

    fn get_class_context_cpp(
        &self,
        node: &Node,
        source: &[u8],
    ) -> Option<String> {
        let mut curr = node.parent();
        while let Some(parent) = curr {
            if parent.kind() == "class_specifier" {
                return parent
                    .child_by_field_name("name")
                    .map(|n| get_node_text(&n, source).to_string());
            }
            curr = parent.parent();
        }
        None
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
            // Unwrap pointers/refs to find identifier
            while let Some(pd) = param_decl {
                match pd.kind() {
                    "identifier" | "field_identifier" | "type_identifier" => {
                        let name = get_node_text(&pd, source).to_string();
                        let type_node = param.child_by_field_name("type");
                        let type_str = type_node
                            .map(|t| get_node_text(&t, source).to_string())
                            .unwrap_or_default();
                        if !name.is_empty() {
                            if !type_str.is_empty() {
                                params.push(format!("{} {}", type_str, name));
                            } else {
                                params.push(name);
                            }
                        }
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

    fn extract_base_classes(&self, class_node: &Node, source: &[u8]) -> Vec<String> {
        let mut bases = Vec::new();
        for i in 0..class_node.child_count() {
            let child = match class_node.child(i) {
                Some(c) => c,
                None => continue,
            };
            if child.kind() == "base_class_clause" {
                for j in 0..child.child_count() {
                    let base_spec = match child.child(j) {
                        Some(b) => b,
                        None => continue,
                    };
                    match base_spec.kind() {
                        "base_class_specifier" => {
                            for k in 0..base_spec.child_count() {
                                if let Some(sub) = base_spec.child(k) {
                                    if matches!(
                                        sub.kind(),
                                        "type_identifier"
                                            | "qualified_identifier"
                                            | "template_type"
                                    ) {
                                        let mut base_name =
                                            get_node_text(&sub, source).to_string();
                                        if let Some(pos) = base_name.find('<') {
                                            base_name.truncate(pos);
                                            base_name = base_name.trim().to_string();
                                        }
                                        bases.push(base_name);
                                        break;
                                    }
                                }
                            }
                        }
                        "type_identifier" | "qualified_identifier" | "template_type" => {
                            let mut base_name =
                                get_node_text(&base_spec, source).to_string();
                            if let Some(pos) = base_name.find('<') {
                                base_name.truncate(pos);
                                base_name = base_name.trim().to_string();
                            }
                            bases.push(base_name);
                        }
                        _ => {}
                    }
                }
                break; // Only one base_class_clause per class
            }
        }
        bases
    }

    fn find_structs_as_classes(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<ClassData> {
        let mut classes = Vec::new();
        for (node, capture_name) in self.execute_query(QUERY_STRUCTS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let struct_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };
            let name = get_node_text(&node, source).to_string();

            let mut class = ClassData {
                name,
                line_number: struct_node.start_position().row + 1,
                end_line: struct_node.end_position().row + 1,
                bases: Vec::new(),
                context: self.get_class_context_cpp(&struct_node, source),
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
        classes
    }

    fn find_enums_as_classes(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<ClassData> {
        let mut classes = Vec::new();
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
        classes
    }

    fn find_unions_as_classes(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<ClassData> {
        let mut classes = Vec::new();
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
        classes
    }

    fn find_macros_as_classes(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<ClassData> {
        let mut classes = Vec::new();
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
}

impl LanguageExtractor for CppExtractor {
    fn language(&self) -> Language {
        tree_sitter_cpp::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "cpp"
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
            if capture_name != "name" && capture_name != "qualified_name" {
                continue;
            }

            // Find enclosing function_definition
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

            let raw_text = get_node_text(&node, source);
            let (name, class_context) =
                if capture_name == "qualified_name" && raw_text.contains("::") {
                    let parts: Vec<&str> = raw_text.rsplitn(2, "::").collect();
                    (parts[0].to_string(), Some(parts[1].to_string()))
                } else {
                    (raw_text.to_string(), None)
                };

            let params = self.extract_function_params(&func_node, source);
            let complexity = self.calculate_complexity(&func_node);

            let (context, context_type, _) =
                self.get_parent_context_cpp(&func_node, source);

            let class_ctx = class_context.or_else(|| self.get_class_context_cpp(&func_node, source));

            let mut func = FunctionData {
                name,
                line_number: node.start_position().row + 1,
                end_line: func_node.end_position().row + 1,
                args: params,
                cyclomatic_complexity: complexity,
                context,
                context_type,
                class_context: class_ctx,
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
            let name = get_node_text(&name_node, source).to_string();
            let bases = self.extract_base_classes(&node, source);

            let mut class = ClassData {
                name,
                line_number: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                bases,
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

        // Also return structs, enums, unions, macros as ClassData
        classes.extend(self.find_structs_as_classes(root, source, index_source));
        classes.extend(self.find_enums_as_classes(root, source, index_source));
        classes.extend(self.find_unions_as_classes(root, source, index_source));
        classes.extend(self.find_macros_as_classes(root, source, index_source));

        classes
    }

    fn find_imports(&self, root: &Node, source: &[u8]) -> Vec<ImportData> {
        let mut imports = Vec::new();

        for (node, capture_name) in self.execute_query(QUERY_IMPORTS, root, source) {
            if capture_name != "path" {
                continue;
            }

            let raw_path = get_node_text(&node, source);
            // Strip quotes or angle brackets
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
            let raw_text = get_node_text(&node, source);
            let mut func_name;
            let mut inferred_obj_type = None;

            match capture_name.as_str() {
                "scoped_name" => {
                    if raw_text.contains("::") {
                        let parts: Vec<&str> = raw_text.rsplitn(2, "::").collect();
                        func_name = parts[0].to_string();
                        inferred_obj_type = Some(parts[1].to_string());
                    } else {
                        func_name = raw_text.to_string();
                    }
                }
                "method_name" => {
                    func_name = raw_text.to_string();
                    // Try to get the object from field_expression parent
                    if let Some(field_expr) = node.parent() {
                        if field_expr.kind() == "field_expression" {
                            if let Some(obj_node) =
                                field_expr.child_by_field_name("argument")
                            {
                                let obj_text = get_node_text(&obj_node, source);
                                if obj_text == "this" {
                                    inferred_obj_type = Some("this".to_string());
                                } else {
                                    inferred_obj_type = Some(obj_text.to_string());
                                }
                            }
                        }
                    }
                }
                "function_name" => {
                    func_name = raw_text.to_string();
                }
                _ => continue,
            }

            let (context_name, context_type, context_line) =
                self.get_parent_context_cpp(&node, source);
            let class_context = self.get_class_context_cpp(&node, source);

            calls.push(CallData {
                name: func_name,
                full_name: raw_text.to_string(),
                line_number: node.start_position().row + 1,
                args: Vec::new(),
                inferred_obj_type,
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

        // First collect lambda assignment names so we can skip them
        let lambda_names: HashSet<String> = self
            .execute_query(QUERY_LAMBDA_ASSIGNMENTS, root, source)
            .iter()
            .filter(|(_, cn)| cn == "name")
            .map(|(n, _)| get_node_text(n, source).to_string())
            .collect();

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

            // Skip lambda assignments
            if lambda_names.contains(&name) {
                continue;
            }

            // Walk up to the declaration node to get type
            let mut decl_node = node.parent();
            while let Some(d) = decl_node {
                if d.kind() == "declaration" || d.kind() == "field_declaration" {
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

            let (context, _, _) = self.get_parent_context_cpp(&node, source);
            let class_context = self.get_class_context_cpp(&node, source);

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
        for (node, capture_name) in self.execute_query(QUERY_PRE_SCAN, root, source) {
            let text = get_node_text(&node, source);
            if capture_name == "qualified_name" {
                // Index both full name and method name
                names.push(text.to_string());
                if text.contains("::") {
                    if let Some(method_name) = text.rsplit("::").next() {
                        names.push(method_name.to_string());
                    }
                }
            } else {
                names.push(text.to_string());
            }
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
        let lang: Language = tree_sitter_cpp::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
void hello(int x, const std::string& name) {
    std::cout << name;
}

int add(int a, int b) {
    return a + b;
}
"#;
        let (tree, source) = parse_source(code);
        let ext = CppExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 2);

        let hello = funcs.iter().find(|f| f.name == "hello").unwrap();
        assert!(hello.args.len() >= 1);

        let add = funcs.iter().find(|f| f.name == "add").unwrap();
        assert_eq!(add.args.len(), 2);
    }

    #[test]
    fn test_find_qualified_function() {
        let code = r#"
void MyClass::doWork(int x) {
    process(x);
}
"#;
        let (tree, source) = parse_source(code);
        let ext = CppExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "doWork");
        assert_eq!(funcs[0].class_context.as_deref(), Some("MyClass"));
    }

    #[test]
    fn test_find_classes_with_bases() {
        let code = r#"
class Animal {
public:
    virtual void speak() = 0;
};

class Dog : public Animal {
public:
    void speak() override {}
};
"#;
        let (tree, source) = parse_source(code);
        let ext = CppExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        // Should include classes plus any structs/enums/unions/macros
        let animal = classes.iter().find(|c| c.name == "Animal").unwrap();
        assert!(animal.bases.is_empty());
        let dog = classes.iter().find(|c| c.name == "Dog").unwrap();
        assert_eq!(dog.bases, vec!["Animal"]);
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
#include <iostream>
#include "myheader.h"
"#;
        let (tree, source) = parse_source(code);
        let ext = CppExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "iostream");
        assert_eq!(imports[1].name, "myheader.h");
    }

    #[test]
    fn test_find_calls() {
        let code = r#"
void test() {
    printf("hello");
    obj.method();
    std::cout << "hi";
}
"#;
        let (tree, source) = parse_source(code);
        let ext = CppExtractor;
        let calls = ext.find_calls(&tree.root_node(), &source);
        assert!(calls.len() >= 1);
        assert!(calls.iter().any(|c| c.name == "printf"));
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
int x = 10;
std::string name = "hello";
"#;
        let (tree, source) = parse_source(code);
        let ext = CppExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert!(vars.len() >= 2);
        assert!(vars.iter().any(|v| v.name == "x"));
        assert!(vars.iter().any(|v| v.name == "name"));
    }

    #[test]
    fn test_pre_scan() {
        let code = r#"
class MyClass {};
struct MyStruct {};
void myFunc() {}
"#;
        let (tree, source) = parse_source(code);
        let ext = CppExtractor;
        let names = ext.pre_scan_definitions(&tree.root_node(), &source);
        assert!(names.contains(&"MyClass".to_string()));
        assert!(names.contains(&"MyStruct".to_string()));
        assert!(names.contains(&"myFunc".to_string()));
    }
}
