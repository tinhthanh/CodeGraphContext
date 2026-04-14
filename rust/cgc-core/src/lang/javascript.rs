use std::collections::HashMap;

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
    (class) @class
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

const QUERY_PRE_SCAN: &str = r#"
    (class_declaration name: (identifier) @name)
    (function_declaration name: (identifier) @name)
    (variable_declarator name: (identifier) @name value: (function_expression))
    (variable_declarator name: (identifier) @name value: (arrow_function))
"#;

/// Context types used for parent context lookups in JavaScript.
const FC_TYPES: &[&str] = &[
    "function_declaration",
    "class_declaration",
    "function_expression",
    "method_definition",
    "arrow_function",
];

pub struct JavaScriptExtractor;

/// Key for grouping captures by function node identity.
type FuncKey = (usize, usize);

/// Bucket that collects captures belonging to a single function.
struct FuncBucket<'a> {
    node: Node<'a>,
    name: Option<String>,
    params: Option<Node<'a>>,
    single_param: Option<Node<'a>>,
}

impl JavaScriptExtractor {
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
                    // Check children for the actual function node
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

    /// Extract JSDoc comment preceding a function node.
    fn get_jsdoc_comment(&self, func_node: &Node, source: &[u8]) -> Option<String> {
        let mut prev = func_node.prev_sibling();
        while let Some(sibling) = prev {
            if sibling.kind() == "comment" {
                let text = get_node_text(&sibling, source);
                if text.starts_with("/**") && text.ends_with("*/") {
                    return Some(text.trim().to_string());
                }
            }
            // Only skip whitespace-like nodes, stop at anything else
            if sibling.kind() != "comment" {
                break;
            }
            prev = sibling.prev_sibling();
        }
        None
    }
}

impl LanguageExtractor for JavaScriptExtractor {
    fn language(&self) -> Language {
        tree_sitter_javascript::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "javascript"
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

            // Resolve name: try captured name, then field name on method_definition
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

            // Parameters
            let args = if let Some(ref params) = data.params {
                self.extract_parameters(params, source)
            } else if let Some(ref single) = data.single_param {
                vec![get_node_text(single, source).to_string()]
            } else {
                Vec::new()
            };

            let (context, context_type, _) = get_parent_context(func_node, source, FC_TYPES);
            let (class_context, _, _) =
                get_parent_context(func_node, source, &["class_declaration"]);

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
                None => continue, // skip anonymous classes
            };
            let name = get_node_text(&name_node, source).to_string();

            // Find base classes via class_heritage node
            let mut bases = Vec::new();
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "class_heritage" {
                        // class_heritage contains extends expressions
                        if child.named_child_count() > 0 {
                            if let Some(base) = child.named_child(0) {
                                bases.push(get_node_text(&base, source).to_string());
                            }
                        } else if child.child_count() > 0 {
                            // Fallback: last child
                            if let Some(base) = child.child(child.child_count() - 1) {
                                bases.push(get_node_text(&base, source).to_string());
                            }
                        }
                    }
                }
            }

            let (context, _, _) = get_parent_context(&node, source, FC_TYPES);

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

            let line_number = node.start_position().row + 1;

            if node.kind() == "import_statement" {
                // ESM import: find the source string node
                let source_node = match node.child_by_field_name("source") {
                    Some(n) => n,
                    None => continue,
                };
                let import_source = get_node_text(&source_node, source)
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_string();

                // Find the import_clause child by iterating (not a field in this grammar version)
                let import_clause = (0..node.child_count())
                    .filter_map(|i| node.child(i))
                    .find(|c| c.kind() == "import_clause");

                match import_clause {
                    None => {
                        // Side-effect import: import './foo'
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
                    Some(clause) => {
                        // import_clause wraps the actual form: look inside it
                        let inner = if clause.kind() == "import_clause" {
                            // Get the first named child inside import_clause
                            (0..clause.named_child_count())
                                .filter_map(|i| clause.named_child(i))
                                .next()
                        } else {
                            Some(clause)
                        };
                        let inner = match inner {
                            Some(n) => n,
                            None => continue,
                        };
                        match inner.kind() {
                            "identifier" => {
                                // Default import: import foo from '...'
                                let alias = get_node_text(&inner, source).to_string();
                                imports.push(ImportData {
                                    name: "default".to_string(),
                                    full_import_name: import_source,
                                    line_number,
                                    alias: Some(alias),
                                    context: (None, None),
                                    lang: self.lang_name().to_string(),
                                    is_dependency: false,
                                });
                            }
                            "namespace_import" => {
                                // import * as name from '...'
                                let alias = inner
                                    .child_by_field_name("alias")
                                    .or_else(|| {
                                        // Fallback: find identifier child
                                        (0..inner.child_count())
                                            .filter_map(|i| inner.child(i))
                                            .find(|c| c.kind() == "identifier")
                                    })
                                    .map(|n| get_node_text(&n, source).to_string());
                                imports.push(ImportData {
                                    name: "*".to_string(),
                                    full_import_name: import_source,
                                    line_number,
                                    alias,
                                    context: (None, None),
                                    lang: self.lang_name().to_string(),
                                    is_dependency: false,
                                });
                            }
                            "named_imports" => {
                                // import { name, name as alias } from '...'
                                for i in 0..inner.child_count() {
                                    if let Some(spec) = inner.child(i) {
                                        if spec.kind() == "import_specifier" {
                                            let name_node = spec.child_by_field_name("name");
                                            let alias_node = spec.child_by_field_name("alias");
                                            if let Some(nn) = name_node {
                                                let original = get_node_text(&nn, source).to_string();
                                                let alias = alias_node.map(|a| get_node_text(&a, source).to_string());
                                                imports.push(ImportData {
                                                    name: original,
                                                    full_import_name: import_source.clone(),
                                                    line_number,
                                                    alias,
                                                    context: (None, None),
                                                    lang: self.lang_name().to_string(),
                                                    is_dependency: false,
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {
                                // Fallback: treat as side-effect
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
            } else if node.kind() == "call_expression" {
                // CommonJS require('...')
                let arguments = match node.child_by_field_name("arguments") {
                    Some(a) => a,
                    None => continue,
                };
                if arguments.named_child_count() == 0 {
                    continue;
                }
                let source_node = match arguments.named_child(0) {
                    Some(n) if n.kind() == "string" => n,
                    _ => continue,
                };
                let require_source = get_node_text(&source_node, source)
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_string();

                let alias = node.parent().and_then(|parent| {
                    if parent.kind() == "variable_declarator" {
                        parent
                            .child_by_field_name("name")
                            .map(|n| get_node_text(&n, source).to_string())
                    } else {
                        None
                    }
                });

                imports.push(ImportData {
                    name: require_source.clone(),
                    full_import_name: require_source,
                    line_number,
                    alias,
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

            // Walk up to find the call_expression or new_expression
            let call_node = {
                let mut current = node.parent();
                loop {
                    match current {
                        Some(n)
                            if n.kind() == "call_expression"
                                || n.kind() == "new_expression" =>
                        {
                            break Some(n);
                        }
                        Some(n) if n.kind() == "program" => break None,
                        Some(n) => current = n.parent(),
                        None => break None,
                    }
                }
            };

            let call_node = match call_node {
                Some(n) => n,
                None => continue,
            };

            let name = get_node_text(&node, source).to_string();
            let full_name = get_node_text(&call_node, source).to_string();

            // Extract arguments
            let mut args = Vec::new();
            if let Some(arguments_node) = call_node.child_by_field_name("arguments") {
                for i in 0..arguments_node.child_count() {
                    if let Some(arg) = arguments_node.child(i) {
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

            let context = get_parent_context(&node, source, FC_TYPES);
            let class_ctx =
                get_parent_context(&node, source, &["class_declaration"]);

            calls.push(CallData {
                name,
                full_name,
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

        calls
    }

    fn find_variables(&self, root: &Node, source: &[u8]) -> Vec<VariableData> {
        let mut variables = Vec::new();

        for (node, capture_name) in self.execute_query(QUERY_VARIABLES, root, source) {
            if capture_name != "name" {
                continue;
            }

            let var_node = match node.parent() {
                Some(p) => p,
                None => continue,
            };

            // Skip variables assigned to functions
            if let Some(value_node) = var_node.child_by_field_name("value") {
                let vt = value_node.kind();
                if vt == "function_expression"
                    || vt == "arrow_function"
                    || vt.contains("function")
                    || vt.contains("arrow")
                {
                    continue;
                }
            }

            let name = get_node_text(&node, source).to_string();

            let value = var_node.child_by_field_name("value").map(|v| {
                if v.kind() == "call_expression" {
                    v.child_by_field_name("function")
                        .map(|f| get_node_text(&f, source).to_string())
                        .unwrap_or_else(|| get_node_text(&v, source).to_string())
                } else {
                    get_node_text(&v, source).to_string()
                }
            });

            let (context, context_type, _) = get_parent_context(&node, source, FC_TYPES);
            let class_context = if context_type.as_deref() == Some("class_declaration") {
                context.clone()
            } else {
                None
            };

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
        let lang: Language = tree_sitter_javascript::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions_declaration() {
        let code = r#"
function hello(name, age) {
    console.log("Hello " + name);
}

function add(a, b) {
    return a + b;
}
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaScriptExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 2);
        let names: Vec<&str> = funcs.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"hello"));
        assert!(names.contains(&"add"));
    }

    #[test]
    fn test_find_functions_arrow() {
        let code = r#"
const greet = (name) => {
    return "Hello " + name;
};

const double = x => x * 2;
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaScriptExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 2);
        let names: Vec<&str> = funcs.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"greet"));
        assert!(names.contains(&"double"));
    }

    #[test]
    fn test_find_classes() {
        let code = r#"
class Animal {
    constructor(name) {
        this.name = name;
    }
}

class Dog extends Animal {
    bark() {
        console.log("Woof!");
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaScriptExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert_eq!(classes.len(), 2);
        let names: Vec<&str> = classes.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Animal"));
        assert!(names.contains(&"Dog"));
        let dog = classes.iter().find(|c| c.name == "Dog").unwrap();
        assert!(!dog.bases.is_empty());
    }

    #[test]
    fn test_find_imports_esm() {
        let code = r#"
import React from 'react';
import { useState, useEffect } from 'react';
import * as path from 'path';
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaScriptExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 4);
    }

    #[test]
    fn test_find_imports_require() {
        let code = r#"
const fs = require('fs');
const path = require('path');
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaScriptExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "fs");
        assert_eq!(imports[0].alias.as_deref(), Some("fs"));
    }

    #[test]
    fn test_find_calls() {
        let code = r#"
function main() {
    console.log("hello");
    const x = new Date();
    fetch("/api");
}
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaScriptExtractor;
        let calls = ext.find_calls(&tree.root_node(), &source);
        assert!(calls.len() >= 3);
        let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"log"));
        assert!(names.contains(&"Date"));
        assert!(names.contains(&"fetch"));
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
const x = 10;
let name = "hello";
const greet = () => {};
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaScriptExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        // greet should be skipped (arrow function assignment)
        assert_eq!(vars.len(), 2);
        let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"x"));
        assert!(names.contains(&"name"));
    }

    #[test]
    fn test_complexity() {
        let code = r#"
function complex(x) {
    if (x > 0) {
        for (let i = 0; i < x; i++) {
            if (i % 2 === 0) {
                console.log(i);
            }
        }
    } else {
        while (x < 0) {
            x++;
        }
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaScriptExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 1);
        // base 1 + if + for + if + while + binary(===) + binary(<) + binary(%) + binary(<) = high
        assert!(funcs[0].cyclomatic_complexity >= 4);
    }

    #[test]
    fn test_pre_scan() {
        let code = r#"
class MyClass {}
function myFunc() {}
const handler = () => {};
"#;
        let (tree, source) = parse_source(code);
        let ext = JavaScriptExtractor;
        let names = ext.pre_scan_definitions(&tree.root_node(), &source);
        assert!(names.contains(&"MyClass".to_string()));
        assert!(names.contains(&"myFunc".to_string()));
        assert!(names.contains(&"handler".to_string()));
    }
}
