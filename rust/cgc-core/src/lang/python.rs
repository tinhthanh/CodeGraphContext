use std::collections::{HashMap, HashSet};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, get_parent_context, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    "if_statement",
    "for_statement",
    "while_statement",
    "except_clause",
    "with_statement",
    "boolean_operator",
    "list_comprehension",
    "generator_expression",
    "case_clause",
];

const QUERY_FUNCTIONS: &str = r#"
    (function_definition
        name: (identifier) @name
        parameters: (parameters) @parameters
        body: (block) @body
        return_type: (_)? @return_type)
"#;

const QUERY_LAMBDA_ASSIGNMENTS: &str = r#"
    (assignment
        left: (identifier) @name
        right: (lambda) @lambda_node)
"#;

const QUERY_CLASSES: &str = r#"
    (class_definition
        name: (identifier) @name
        superclasses: (argument_list)? @superclasses
        body: (block) @body)
"#;

const QUERY_IMPORTS: &str = r#"
    (import_statement name: (_) @import)
    (import_from_statement) @from_import_stmt
"#;

const QUERY_CALLS: &str = r#"
    (call
        function: (identifier) @name)
    (call
        function: (attribute attribute: (identifier) @name) @full_call)
"#;

const QUERY_VARIABLES: &str = r#"
    (assignment
        left: (identifier) @name)
"#;

const QUERY_DICT_METHOD_REFS: &str = r#"
    (dictionary
        (pair
            key: (_) @key
            value: (attribute) @method_ref))
"#;

const QUERY_PRE_SCAN: &str = r#"
    (class_definition name: (identifier) @name)
    (function_definition name: (identifier) @name)
"#;

pub struct PythonExtractor;

impl PythonExtractor {
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

    fn get_docstring(&self, body_node: &Node, source: &[u8]) -> Option<String> {
        if body_node.child_count() == 0 {
            return None;
        }
        let first_child = body_node.child(0)?;
        if first_child.kind() != "expression_statement" {
            return None;
        }
        let string_node = first_child.child(0)?;
        if string_node.kind() != "string" {
            return None;
        }
        let text = get_node_text(&string_node, source);
        // Strip quotes
        let trimmed = text
            .strip_prefix("\"\"\"")
            .and_then(|s| s.strip_suffix("\"\"\""))
            .or_else(|| {
                text.strip_prefix("'''")
                    .and_then(|s| s.strip_suffix("'''"))
            })
            .or_else(|| text.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
            .or_else(|| text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(text);
        Some(trimmed.to_string())
    }

    fn find_lambda_assignments(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<FunctionData> {
        let mut functions = Vec::new();
        let fc_types = &["function_definition", "class_definition"];

        for (node, capture_name) in self.execute_query(QUERY_LAMBDA_ASSIGNMENTS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let assignment_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };
            let lambda_node = match assignment_node.child_by_field_name("right") {
                Some(n) => n,
                None => continue,
            };
            let name = get_node_text(&node, source).to_string();
            let params_node = lambda_node.child_by_field_name("parameters");

            let (context, context_type, _) = get_parent_context(&assignment_node, source, fc_types);
            let (class_context, _, _) =
                get_parent_context(&assignment_node, source, &["class_definition"]);

            let args = match params_node {
                Some(ref pn) => {
                    let mut a = Vec::new();
                    for i in 0..pn.child_count() {
                        if let Some(child) = pn.child(i) {
                            if child.kind() == "identifier" {
                                a.push(get_node_text(&child, source).to_string());
                            }
                        }
                    }
                    a
                }
                None => Vec::new(),
            };

            let mut func = FunctionData {
                name,
                line_number: node.start_position().row + 1,
                end_line: assignment_node.end_position().row + 1,
                args,
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
                func.source = Some(get_node_text(&assignment_node, source).to_string());
            }

            functions.push(func);
        }
        functions
    }

    fn find_dict_method_references(&self, root: &Node, source: &[u8]) -> Vec<CallData> {
        let mut calls = Vec::new();
        let fc_types = &["function_definition", "class_definition"];

        // Track dict assignments: var_name -> (methods, context)
        let mut dict_assignments: HashMap<String, (Vec<(String, String, usize)>, (Option<String>, Option<String>, Option<usize>))> =
            HashMap::new();

        for (node, capture_name) in self.execute_query(QUERY_DICT_METHOD_REFS, root, source) {
            if capture_name != "method_ref" {
                continue;
            }

            // Walk up to dictionary node
            let mut dict_node = node.parent();
            while let Some(dn) = dict_node {
                if dn.kind() == "dictionary" {
                    break;
                }
                dict_node = dn.parent();
            }
            let dict_node = match dict_node {
                Some(dn) if dn.kind() == "dictionary" => dn,
                _ => continue,
            };

            let assignment_node = match dict_node.parent() {
                Some(p) if p.kind() == "assignment" => p,
                _ => continue,
            };

            let left_node = match assignment_node.child_by_field_name("left") {
                Some(n) => n,
                None => continue,
            };

            let var_name = get_node_text(&left_node, source).to_string();
            let method_ref = get_node_text(&node, source);
            let method_name = method_ref
                .rsplit('.')
                .next()
                .unwrap_or(method_ref)
                .to_string();

            let entry = dict_assignments
                .entry(var_name)
                .or_insert_with(|| {
                    let ctx = get_parent_context(&assignment_node, source, fc_types);
                    (Vec::new(), ctx)
                });
            entry.0.push((
                method_name,
                method_ref.to_string(),
                node.start_position().row + 1,
            ));
        }

        for (_var, (methods, (context, context_type, context_line))) in dict_assignments {
            for (name, full_name, line_number) in methods {
                calls.push(CallData {
                    name,
                    full_name,
                    line_number,
                    args: Vec::new(),
                    inferred_obj_type: None,
                    context: (context.clone(), context_type.clone(), context_line),
                    class_context: (None, None),
                    lang: self.lang_name().to_string(),
                    is_dependency: false,
                    is_indirect_call: true,
                });
            }
        }

        calls
    }
}

impl LanguageExtractor for PythonExtractor {
    fn language(&self) -> Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "python"
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
        let fc_types = &["function_definition", "class_definition"];

        for (node, capture_name) in self.execute_query(QUERY_FUNCTIONS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let func_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };
            let name = get_node_text(&node, source).to_string();
            let params_node = func_node.child_by_field_name("parameters");
            let body_node = func_node.child_by_field_name("body");

            // Decorators
            let mut decorators = Vec::new();
            for i in 0..func_node.child_count() {
                if let Some(child) = func_node.child(i) {
                    if child.kind() == "decorator" {
                        decorators.push(get_node_text(&child, source).to_string());
                    }
                }
            }

            let (context, context_type, _) = get_parent_context(&func_node, source, fc_types);
            let (class_context, _, _) =
                get_parent_context(&func_node, source, &["class_definition"]);

            // Parse parameters
            let args = parse_python_params(&params_node, source);

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
                decorators,
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                func.source = Some(get_node_text(&func_node, source).to_string());
                func.docstring = body_node.and_then(|b| self.get_docstring(&b, source));
            }

            functions.push(func);
        }

        // Also find lambda assignments
        functions.extend(self.find_lambda_assignments(root, source, index_source));

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
            let body_node = class_node.child_by_field_name("body");
            let superclasses_node = class_node.child_by_field_name("superclasses");

            let mut bases = Vec::new();
            if let Some(ref sc) = superclasses_node {
                for i in 0..sc.child_count() {
                    if let Some(child) = sc.child(i) {
                        if child.kind() == "identifier" || child.kind() == "attribute" {
                            bases.push(get_node_text(&child, source).to_string());
                        }
                    }
                }
            }

            let mut decorators = Vec::new();
            for i in 0..class_node.child_count() {
                if let Some(child) = class_node.child(i) {
                    if child.kind() == "decorator" {
                        decorators.push(get_node_text(&child, source).to_string());
                    }
                }
            }

            let (context, _, _) = get_parent_context(
                &class_node,
                source,
                &["function_definition", "class_definition"],
            );

            let mut class = ClassData {
                name,
                line_number: node.start_position().row + 1,
                end_line: class_node.end_position().row + 1,
                bases,
                context,
                decorators,
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&class_node, source).to_string());
                class.docstring = body_node.and_then(|b| self.get_docstring(&b, source));
            }

            classes.push(class);
        }

        classes
    }

    fn find_imports(&self, root: &Node, source: &[u8]) -> Vec<ImportData> {
        let mut imports = Vec::new();
        let mut seen_modules: HashSet<String> = HashSet::new();
        let fc_types = &["function_definition", "class_definition"];

        for (node, capture_name) in self.execute_query(QUERY_IMPORTS, root, source) {
            match capture_name.as_str() {
                "import" => {
                    let text = get_node_text(&node, source);
                    let (full_name, alias) = if let Some(pos) = text.find(" as ") {
                        (
                            text[..pos].trim().to_string(),
                            Some(text[pos + 4..].trim().to_string()),
                        )
                    } else {
                        (text.trim().to_string(), None)
                    };

                    if seen_modules.contains(&full_name) {
                        continue;
                    }
                    seen_modules.insert(full_name.clone());

                    let ctx = get_parent_context(&node, source, fc_types);

                    imports.push(ImportData {
                        name: full_name.clone(),
                        full_import_name: full_name,
                        line_number: node.start_position().row + 1,
                        alias,
                        context: (ctx.0, ctx.1),
                        lang: self.lang_name().to_string(),
                        is_dependency: false,
                    });
                }
                "from_import_stmt" => {
                    let module_name_node = match node.child_by_field_name("module_name") {
                        Some(n) => n,
                        None => continue,
                    };
                    let module_name = get_node_text(&module_name_node, source);

                    let import_list_node = match node.child_by_field_name("name") {
                        Some(n) => n,
                        None => continue,
                    };

                    // Collect import items: either the node itself is the import,
                    // or it's a container with multiple children
                    let items: Vec<Node> = match import_list_node.kind() {
                        "aliased_import" | "dotted_name" | "identifier" => {
                            vec![import_list_node]
                        }
                        _ => {
                            (0..import_list_node.child_count())
                                .filter_map(|i| import_list_node.child(i))
                                .collect()
                        }
                    };

                    for child in items {
                        let (imported_name, alias) = if child.kind() == "aliased_import" {
                            let name = child
                                .child_by_field_name("name")
                                .map(|n| get_node_text(&n, source).to_string());
                            let alias = child
                                .child_by_field_name("alias")
                                .map(|n| get_node_text(&n, source).to_string());
                            (name, alias)
                        } else if child.kind() == "dotted_name" || child.kind() == "identifier" {
                            (Some(get_node_text(&child, source).to_string()), None)
                        } else {
                            continue;
                        };

                        if let Some(imported_name) = imported_name {
                            let full_import_name =
                                format!("{module_name}.{imported_name}");
                            if seen_modules.contains(&full_import_name) {
                                continue;
                            }
                            seen_modules.insert(full_import_name.clone());

                            let ctx = get_parent_context(&child, source, fc_types);

                            imports.push(ImportData {
                                name: imported_name,
                                full_import_name,
                                line_number: child.start_position().row + 1,
                                alias,
                                context: (ctx.0, ctx.1),
                                lang: self.lang_name().to_string(),
                                is_dependency: false,
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        imports
    }

    fn find_calls(&self, root: &Node, source: &[u8]) -> Vec<CallData> {
        let mut calls = Vec::new();
        let fc_types = &["function_definition", "class_definition"];

        for (node, capture_name) in self.execute_query(QUERY_CALLS, root, source) {
            if capture_name != "name" {
                continue;
            }

            // Navigate to the call node
            let call_node = {
                let parent = match node.parent() {
                    Some(p) => p,
                    None => continue,
                };
                if parent.kind() == "call" {
                    parent
                } else {
                    match parent.parent() {
                        Some(gp) if gp.kind() == "call" => gp,
                        _ => continue,
                    }
                }
            };

            let full_call_node = match call_node.child_by_field_name("function") {
                Some(n) => n,
                None => continue,
            };

            // Parse arguments
            let mut args = Vec::new();
            if let Some(arguments_node) = call_node.child_by_field_name("arguments") {
                for i in 0..arguments_node.child_count() {
                    if let Some(arg) = arguments_node.child(i) {
                        let text = get_node_text(&arg, source);
                        if !text.is_empty() && text != "(" && text != ")" && text != "," {
                            args.push(text.to_string());
                        }
                    }
                }
            }

            let context = get_parent_context(&node, source, fc_types);
            let class_ctx = get_parent_context(&node, source, &["class_definition"]);

            calls.push(CallData {
                name: get_node_text(&node, source).to_string(),
                full_name: get_node_text(&full_call_node, source).to_string(),
                line_number: node.start_position().row + 1,
                args,
                inferred_obj_type: None,
                context,
                class_context: (class_ctx.0, class_ctx.1),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                is_indirect_call: false,
            });
        }

        // Add dict method references
        calls.extend(self.find_dict_method_references(root, source));

        calls
    }

    fn find_variables(&self, root: &Node, source: &[u8]) -> Vec<VariableData> {
        let mut variables = Vec::new();

        for (node, capture_name) in self.execute_query(QUERY_VARIABLES, root, source) {
            if capture_name != "name" {
                continue;
            }

            let assignment_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };

            // Skip lambda assignments
            if let Some(right) = assignment_node.child_by_field_name("right") {
                if right.kind() == "lambda" {
                    continue;
                }
            }

            let name = get_node_text(&node, source).to_string();
            let value = assignment_node
                .child_by_field_name("right")
                .map(|n| get_node_text(&n, source).to_string());
            let type_annotation = assignment_node
                .child_by_field_name("type")
                .map(|n| get_node_text(&n, source).to_string());

            let (context, _, _) = get_parent_context(
                &node,
                source,
                &["function_definition", "class_definition"],
            );
            let (class_context, _, _) =
                get_parent_context(&node, source, &["class_definition"]);

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

/// Parse Python function parameters from a parameters node.
fn parse_python_params(params_node: &Option<Node>, source: &[u8]) -> Vec<String> {
    let params_node = match params_node {
        Some(n) => n,
        None => return Vec::new(),
    };

    let mut args = Vec::new();
    for i in 0..params_node.child_count() {
        let child = match params_node.child(i) {
            Some(c) => c,
            None => continue,
        };
        let arg_text = match child.kind() {
            "identifier" => Some(get_node_text(&child, source).to_string()),
            "default_parameter" | "typed_default_parameter" => child
                .child_by_field_name("name")
                .map(|n| get_node_text(&n, source).to_string()),
            "typed_parameter" => {
                // typed_parameter: first named child is the identifier
                child
                    .child_by_field_name("name")
                    .or_else(|| {
                        // Fallback: find first identifier child
                        (0..child.child_count())
                            .filter_map(|i| child.child(i))
                            .find(|c| c.kind() == "identifier")
                    })
                    .map(|n| get_node_text(&n, source).to_string())
            }
            "list_splat_pattern" | "dictionary_splat_pattern" => {
                Some(get_node_text(&child, source).to_string())
            }
            _ => None,
        };
        if let Some(text) = arg_text {
            if !text.is_empty() {
                args.push(text);
            }
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_source(code: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let mut parser = Parser::new();
        let lang: Language = tree_sitter_python::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
def hello(name, age=10):
    """Say hello."""
    print(f"Hello {name}")

def add(a: int, b: int) -> int:
    return a + b
"#;
        let (tree, source) = parse_source(code);
        let ext = PythonExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, true);
        assert_eq!(funcs.len(), 2);
        assert_eq!(funcs[0].name, "hello");
        assert_eq!(funcs[0].args, vec!["name", "age"]);
        assert_eq!(funcs[0].docstring.as_deref(), Some("Say hello."));
        assert_eq!(funcs[1].name, "add");
        assert_eq!(funcs[1].args, vec!["a", "b"]);
    }

    #[test]
    fn test_find_classes() {
        let code = r#"
class Animal:
    """Base animal."""
    pass

class Dog(Animal):
    def bark(self):
        pass
"#;
        let (tree, source) = parse_source(code);
        let ext = PythonExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, true);
        assert_eq!(classes.len(), 2);
        assert_eq!(classes[0].name, "Animal");
        assert!(classes[0].bases.is_empty());
        assert_eq!(classes[0].docstring.as_deref(), Some("Base animal."));
        assert_eq!(classes[1].name, "Dog");
        assert_eq!(classes[1].bases, vec!["Animal"]);
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
import os
import sys as system
from pathlib import Path
from collections import OrderedDict as OD
"#;
        let (tree, source) = parse_source(code);
        let ext = PythonExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 4);
        assert_eq!(imports[0].name, "os");
        assert_eq!(imports[1].name, "sys");
        assert_eq!(imports[1].alias.as_deref(), Some("system"));
        assert_eq!(imports[2].name, "Path");
        assert_eq!(imports[2].full_import_name, "pathlib.Path");
        assert_eq!(imports[3].name, "OrderedDict");
        assert_eq!(imports[3].alias.as_deref(), Some("OD"));
    }

    #[test]
    fn test_find_calls() {
        let code = r#"
def main():
    print("hello")
    os.path.join("a", "b")
"#;
        let (tree, source) = parse_source(code);
        let ext = PythonExtractor;
        let calls = ext.find_calls(&tree.root_node(), &source);
        assert!(calls.len() >= 2);
        assert_eq!(calls[0].name, "print");
        assert_eq!(calls[1].name, "join");
        assert_eq!(calls[1].full_name, "os.path.join");
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
x = 10
name = "hello"
"#;
        let (tree, source) = parse_source(code);
        let ext = PythonExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].name, "x");
        assert_eq!(vars[0].value.as_deref(), Some("10"));
        assert_eq!(vars[1].name, "name");
    }

    #[test]
    fn test_pre_scan() {
        let code = r#"
class MyClass:
    pass

def my_func():
    pass

def nested_outer():
    def nested_inner():
        pass
"#;
        let (tree, source) = parse_source(code);
        let ext = PythonExtractor;
        let names = ext.pre_scan_definitions(&tree.root_node(), &source);
        assert!(names.contains(&"MyClass".to_string()));
        assert!(names.contains(&"my_func".to_string()));
        assert!(names.contains(&"nested_outer".to_string()));
        // pre_scan captures all names from query, including nested
        assert!(names.contains(&"nested_inner".to_string()));
    }

    #[test]
    fn test_complexity() {
        let code = r#"
def complex_func(x):
    if x > 0:
        for i in range(x):
            if i % 2 == 0:
                print(i)
    else:
        while x < 0:
            x += 1
"#;
        let (tree, source) = parse_source(code);
        let ext = PythonExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 1);
        // base 1 + if + for + if + while = 5
        assert!(funcs[0].cyclomatic_complexity >= 4);
    }
}
