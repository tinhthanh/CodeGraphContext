use std::collections::HashMap;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, get_parent_context, LanguageExtractor};
use crate::types::*;

/// TSX (TypeScript with JSX) extractor.
/// Reuses the same queries and logic as TypeScript, but uses `LANGUAGE_TSX`
/// so JSX syntax is parsed correctly.

const COMPLEXITY_TYPES: &[&str] = &[
    "if_statement",
    "for_statement",
    "while_statement",
    "do_statement",
    "switch_statement",
    "case_statement",
    "conditional_expression",
    "logical_expression",
    "binary_expression",
    "catch_clause",
];

const QUERY_FUNCTIONS: &str = r#"
    (function_declaration
        name: (identifier) @name
        parameters: (formal_parameters) @params
    ) @function_node

    (variable_declarator
        name: (identifier) @name
        value: (function_expression
            parameters: (formal_parameters) @params
        ) @function_node
    )

    (variable_declarator
        name: (identifier) @name
        value: (arrow_function
            parameters: (formal_parameters) @params
        ) @function_node
    )

    (variable_declarator
        name: (identifier) @name
        value: (arrow_function
            parameter: (identifier) @single_param
        ) @function_node
    )

    (method_definition
        name: (property_identifier) @name
        parameters: (formal_parameters) @params
    ) @function_node

    (assignment_expression
        left: (member_expression
            property: (property_identifier) @name
        )
        right: (function_expression
            parameters: (formal_parameters) @params
        ) @function_node
    )

    (assignment_expression
        left: (member_expression
            property: (property_identifier) @name
        )
        right: (arrow_function
            parameters: (formal_parameters) @params
        ) @function_node
    )
"#;

const QUERY_CLASSES: &str = r#"
    (class_declaration) @class
    (abstract_class_declaration) @class
    (class) @class
"#;

const QUERY_INTERFACES: &str = r#"
    (interface_declaration
        name: (type_identifier) @name
    ) @interface_node
"#;

const QUERY_TYPE_ALIASES: &str = r#"
    (type_alias_declaration
        name: (type_identifier) @name
    ) @type_alias_node
"#;

const QUERY_IMPORTS: &str = r#"
    (import_statement) @import
    (call_expression
        function: (identifier) @require_call (#eq? @require_call "require")
    ) @import
"#;

const QUERY_CALLS: &str = r#"
    (call_expression function: (identifier) @name)
    (call_expression function: (member_expression property: (property_identifier) @name))
    (new_expression constructor: (identifier) @name)
    (new_expression constructor: (member_expression property: (property_identifier) @name))
"#;

const QUERY_VARIABLES: &str = r#"
    (variable_declarator name: (identifier) @name)
"#;

/// Context types for parent lookups in TSX/TypeScript.
const FC_TYPES: &[&str] = &[
    "function_declaration",
    "class_declaration",
    "abstract_class_declaration",
    "function_expression",
    "method_definition",
    "arrow_function",
];

/// Key for grouping captures by function node identity.
type FuncKey = (usize, usize);

/// Bucket that collects captures belonging to a single function.
struct FuncBucket<'a> {
    node: Node<'a>,
    name: Option<String>,
    params: Option<Node<'a>>,
    single_param: Option<Node<'a>>,
}

pub struct TsxExtractor;

impl TsxExtractor {
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

    /// Walk up from a name node to find its owning function node.
    fn find_function_node_for_name<'a>(&self, name_node: &Node<'a>) -> Option<Node<'a>> {
        let mut current = name_node.parent();
        while let Some(node) = current {
            match node.kind() {
                "function_declaration" | "function_expression" | "arrow_function"
                | "method_definition" => return Some(node),
                "variable_declarator" | "assignment_expression" => {
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if matches!(
                                child.kind(),
                                "function_expression" | "arrow_function"
                            ) {
                                return Some(child);
                            }
                        }
                    }
                }
                _ => {}
            }
            current = node.parent();
        }
        None
    }

    /// Walk up from a params node to find its owning function node.
    fn find_function_node_for_params<'a>(&self, params_node: &Node<'a>) -> Option<Node<'a>> {
        let mut current = params_node.parent();
        while let Some(node) = current {
            if matches!(
                node.kind(),
                "function_declaration"
                    | "function_expression"
                    | "arrow_function"
                    | "method_definition"
            ) {
                return Some(node);
            }
            current = node.parent();
        }
        None
    }

    /// Extract parameter names from a formal_parameters node.
    fn extract_parameters(&self, params_node: &Node, source: &[u8]) -> Vec<String> {
        let mut params = Vec::new();
        if params_node.kind() != "formal_parameters" {
            return params;
        }
        for i in 0..params_node.child_count() {
            let child = match params_node.child(i) {
                Some(c) => c,
                None => continue,
            };
            match child.kind() {
                "identifier" => {
                    params.push(get_node_text(&child, source).to_string());
                }
                "required_parameter" => {
                    if let Some(pattern) = child.child_by_field_name("pattern") {
                        params.push(get_node_text(&pattern, source).to_string());
                    } else {
                        for j in 0..child.child_count() {
                            if let Some(sub) = child.child(j) {
                                if matches!(
                                    sub.kind(),
                                    "identifier" | "object_pattern" | "array_pattern"
                                ) {
                                    params.push(get_node_text(&sub, source).to_string());
                                    break;
                                }
                            }
                        }
                    }
                }
                "optional_parameter" => {
                    if let Some(pattern) = child.child_by_field_name("pattern") {
                        params.push(get_node_text(&pattern, source).to_string());
                    }
                }
                "assignment_pattern" => {
                    if let Some(left) = child.child_by_field_name("left") {
                        if left.kind() == "identifier" {
                            params.push(get_node_text(&left, source).to_string());
                        }
                    }
                }
                "rest_pattern" => {
                    if let Some(arg) = child.child_by_field_name("argument") {
                        if arg.kind() == "identifier" {
                            params.push(format!("...{}", get_node_text(&arg, source)));
                        }
                    }
                }
                _ => {}
            }
        }
        params
    }

    /// Process an import_clause node.
    fn process_import_clause(
        &self,
        clause: &Node,
        import_source: &str,
        line_number: usize,
        source: &[u8],
        imports: &mut Vec<ImportData>,
    ) {
        for i in 0..clause.child_count() {
            let child = match clause.child(i) {
                Some(c) => c,
                None => continue,
            };
            match child.kind() {
                "identifier" => {
                    let alias = get_node_text(&child, source).to_string();
                    imports.push(ImportData {
                        name: "default".to_string(),
                        full_import_name: import_source.to_string(),
                        line_number,
                        alias: Some(alias),
                        context: (None, None),
                        lang: self.lang_name().to_string(),
                        is_dependency: false,
                    });
                }
                "namespace_import" => {
                    let alias_node = (0..child.child_count())
                        .filter_map(|j| child.child(j))
                        .find(|c| c.kind() == "identifier");
                    if let Some(alias_n) = alias_node {
                        let alias = get_node_text(&alias_n, source).to_string();
                        imports.push(ImportData {
                            name: "*".to_string(),
                            full_import_name: import_source.to_string(),
                            line_number,
                            alias: Some(alias),
                            context: (None, None),
                            lang: self.lang_name().to_string(),
                            is_dependency: false,
                        });
                    }
                }
                "named_imports" => {
                    for j in 0..child.child_count() {
                        if let Some(specifier) = child.child(j) {
                            if specifier.kind() == "import_specifier" {
                                let spec_name = specifier
                                    .child_by_field_name("name")
                                    .map(|n| get_node_text(&n, source).to_string());
                                let spec_alias = specifier
                                    .child_by_field_name("alias")
                                    .map(|n| get_node_text(&n, source).to_string());
                                if let Some(imported_name) = spec_name {
                                    imports.push(ImportData {
                                        name: imported_name,
                                        full_import_name: import_source.to_string(),
                                        line_number,
                                        alias: spec_alias,
                                        context: (None, None),
                                        lang: self.lang_name().to_string(),
                                        is_dependency: false,
                                    });
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Extract JSDoc comment preceding a node.
    fn get_jsdoc_comment(&self, node: &Node, source: &[u8]) -> Option<String> {
        let mut prev = node.prev_sibling();
        while let Some(sibling) = prev {
            if sibling.kind() == "comment" {
                let text = get_node_text(&sibling, source);
                if text.starts_with("/**") && text.ends_with("*/") {
                    return Some(text.trim().to_string());
                }
            }
            if sibling.kind() != "comment" {
                break;
            }
            prev = sibling.prev_sibling();
        }
        None
    }
}

impl LanguageExtractor for TsxExtractor {
    fn language(&self) -> Language {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    }

    fn lang_name(&self) -> &str {
        "tsx"
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
        let mut buckets: HashMap<FuncKey, FuncBucket> = HashMap::new();

        for (node, capture_name) in self.execute_query(QUERY_FUNCTIONS, root, source) {
            match capture_name.as_str() {
                "function_node" => {
                    let key = (node.start_byte(), node.end_byte());
                    buckets.entry(key).or_insert(FuncBucket {
                        node,
                        name: None,
                        params: None,
                        single_param: None,
                    });
                }
                "name" => {
                    if let Some(fn_node) = self.find_function_node_for_name(&node) {
                        let key = (fn_node.start_byte(), fn_node.end_byte());
                        let bucket = buckets.entry(key).or_insert(FuncBucket {
                            node: fn_node,
                            name: None,
                            params: None,
                            single_param: None,
                        });
                        bucket.name = Some(get_node_text(&node, source).to_string());
                    }
                }
                "params" => {
                    if let Some(fn_node) = self.find_function_node_for_params(&node) {
                        let key = (fn_node.start_byte(), fn_node.end_byte());
                        let bucket = buckets.entry(key).or_insert(FuncBucket {
                            node: fn_node,
                            name: None,
                            params: None,
                            single_param: None,
                        });
                        bucket.params = Some(node);
                    }
                }
                "single_param" => {
                    if let Some(fn_node) = self.find_function_node_for_params(&node) {
                        let key = (fn_node.start_byte(), fn_node.end_byte());
                        let bucket = buckets.entry(key).or_insert(FuncBucket {
                            node: fn_node,
                            name: None,
                            params: None,
                            single_param: None,
                        });
                        bucket.single_param = Some(node);
                    }
                }
                _ => {}
            }
        }

        let mut functions = Vec::new();
        for (_, data) in &buckets {
            let func_node = &data.node;

            let name = match &data.name {
                Some(n) => n.clone(),
                None => {
                    if func_node.kind() == "method_definition" {
                        match func_node.child_by_field_name("name") {
                            Some(n) => get_node_text(&n, source).to_string(),
                            None => continue,
                        }
                    } else {
                        continue;
                    }
                }
            };

            let args = if let Some(ref params) = data.params {
                self.extract_parameters(params, source)
            } else if let Some(ref single) = data.single_param {
                vec![get_node_text(single, source).to_string()]
            } else {
                Vec::new()
            };

            let (context, context_type, _) = get_parent_context(func_node, source, FC_TYPES);
            let (class_context, _, _) = get_parent_context(
                func_node,
                source,
                &["class_declaration", "abstract_class_declaration"],
            );

            let complexity = self.calculate_complexity(func_node);
            let docstring = if index_source {
                self.get_jsdoc_comment(func_node, source)
            } else {
                None
            };

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
                docstring,
            };

            if index_source {
                func.source = Some(get_node_text(func_node, source).to_string());
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
            if capture_name != "class" {
                continue;
            }

            let name_node = match node.child_by_field_name("name") {
                Some(n) => n,
                None => continue,
            };
            let name = get_node_text(&name_node, source).to_string();

            // Extract bases from heritage clause
            let mut bases = Vec::new();
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "class_heritage" {
                        for j in 0..child.child_count() {
                            if let Some(sub) = child.child(j) {
                                if sub.kind() == "extends_clause"
                                    || sub.kind() == "implements_clause"
                                {
                                    for k in 0..sub.child_count() {
                                        if let Some(type_node) = sub.child(k) {
                                            if type_node.kind() == "identifier"
                                                || type_node.kind() == "type_identifier"
                                            {
                                                bases.push(
                                                    get_node_text(&type_node, source)
                                                        .to_string(),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let (context, _, _) = get_parent_context(&node, source, FC_TYPES);

            let docstring = if index_source {
                self.get_jsdoc_comment(&node, source)
            } else {
                None
            };

            let mut class = ClassData {
                name,
                line_number: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                bases,
                context,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring,
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

            if node.kind() == "import_statement" {
                // Find the source string
                let mut import_source = String::new();
                if let Some(src_node) = node.child_by_field_name("source") {
                    import_source = get_node_text(&src_node, source)
                        .trim_matches(|c| c == '\'' || c == '"')
                        .to_string();
                }

                let line_number = node.start_position().row + 1;

                // Process import clause
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        if child.kind() == "import_clause" {
                            self.process_import_clause(
                                &child,
                                &import_source,
                                line_number,
                                source,
                                &mut imports,
                            );
                        }
                    }
                }

                // Side-effect import (no clause)
                if imports.is_empty()
                    || imports.last().map(|i| i.line_number) != Some(node.start_position().row + 1)
                {
                    if !import_source.is_empty() {
                        let has_clause = (0..node.child_count())
                            .filter_map(|i| node.child(i))
                            .any(|c| c.kind() == "import_clause");
                        if !has_clause {
                            imports.push(ImportData {
                                name: import_source.clone(),
                                full_import_name: import_source,
                                line_number,
                                alias: None,
                                context: (None, None),
                                lang: self.lang_name().to_string(),
                                is_dependency: false,
                            });
                        }
                    }
                }
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

            // Navigate to the call/new expression node
            let call_node = {
                let parent = match node.parent() {
                    Some(p) => p,
                    None => continue,
                };
                if parent.kind() == "call_expression" || parent.kind() == "new_expression" {
                    parent
                } else {
                    match parent.parent() {
                        Some(gp)
                            if gp.kind() == "call_expression"
                                || gp.kind() == "new_expression" =>
                        {
                            gp
                        }
                        _ => continue,
                    }
                }
            };

            let full_call_node = call_node
                .child_by_field_name("function")
                .or_else(|| call_node.child_by_field_name("constructor"));
            let full_name = match full_call_node {
                Some(ref n) => get_node_text(n, source).to_string(),
                None => get_node_text(&node, source).to_string(),
            };

            let context = get_parent_context(&node, source, FC_TYPES);
            let class_ctx = get_parent_context(
                &node,
                source,
                &["class_declaration", "abstract_class_declaration"],
            );

            calls.push(CallData {
                name: get_node_text(&node, source).to_string(),
                full_name,
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

            let declarator = match node.parent() {
                Some(p) => p,
                None => continue,
            };

            // Skip function/arrow function assignments (already captured as functions)
            if let Some(value) = declarator.child_by_field_name("value") {
                if matches!(
                    value.kind(),
                    "function_expression" | "arrow_function" | "function"
                ) {
                    continue;
                }
            }

            let name = get_node_text(&node, source).to_string();
            let value = declarator
                .child_by_field_name("value")
                .map(|n| get_node_text(&n, source).to_string());
            let type_annotation = declarator
                .child_by_field_name("type")
                .map(|n| get_node_text(&n, source).to_string());

            let (context, _, _) = get_parent_context(&node, source, FC_TYPES);
            let (class_context, _, _) = get_parent_context(
                &node,
                source,
                &["class_declaration", "abstract_class_declaration"],
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
        let lang: Language = tree_sitter_typescript::LANGUAGE_TSX.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
function greet(name: string): string {
    return `Hello ${name}`;
}

const add = (a: number, b: number): number => a + b;
"#;
        let (tree, source) = parse_source(code);
        let ext = TsxExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert!(funcs.len() >= 2);
    }

    #[test]
    fn test_find_jsx_component() {
        let code = r#"
import React from 'react';

const MyComponent: React.FC<Props> = ({ name }) => {
    return <div>Hello {name}</div>;
};

function App() {
    return <MyComponent name="world" />;
}
"#;
        let (tree, source) = parse_source(code);
        let ext = TsxExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert!(funcs.len() >= 2);
    }

    #[test]
    fn test_find_classes() {
        let code = r#"
class Animal {
    name: string;
    constructor(name: string) {
        this.name = name;
    }
}

class Dog extends Animal {
    bark(): string {
        return "Woof!";
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = TsxExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.len() >= 2);
        assert_eq!(classes[0].name, "Animal");
        assert_eq!(classes[1].name, "Dog");
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
import React from 'react';
import { useState, useEffect } from 'react';
import * as Utils from './utils';
"#;
        let (tree, source) = parse_source(code);
        let ext = TsxExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 3);
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
const x: number = 10;
let name = "hello";
"#;
        let (tree, source) = parse_source(code);
        let ext = TsxExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert!(vars.len() >= 2);
        assert_eq!(vars[0].name, "x");
    }
}
