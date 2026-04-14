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

const QUERY_PRE_SCAN: &str = r#"
    (class_declaration name: (type_identifier) @name)
    (abstract_class_declaration name: (type_identifier) @name)
    (function_declaration name: (identifier) @name)
    (variable_declarator name: (identifier) @name value: (function_expression))
    (variable_declarator name: (identifier) @name value: (arrow_function))
    (interface_declaration name: (type_identifier) @name)
    (type_alias_declaration name: (type_identifier) @name)
"#;

/// Context types used for parent context lookups in TypeScript.
const FC_TYPES: &[&str] = &[
    "function_declaration",
    "class_declaration",
    "abstract_class_declaration",
    "function_expression",
    "method_definition",
    "arrow_function",
];

pub struct TypeScriptExtractor;

/// Key for grouping captures by function node identity.
type FuncKey = (usize, usize);

/// Bucket that collects captures belonging to a single function.
struct FuncBucket<'a> {
    node: Node<'a>,
    name: Option<String>,
    params: Option<Node<'a>>,
    single_param: Option<Node<'a>>,
}

impl TypeScriptExtractor {
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
    /// Handles TypeScript-specific parameter types: required_parameter, optional_parameter.
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
                    // required_parameter -> pattern (identifier) + type_annotation
                    if let Some(pattern) = child.child_by_field_name("pattern") {
                        params.push(get_node_text(&pattern, source).to_string());
                    } else {
                        // Fallback: find first identifier-like child
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

    /// Process an import_clause node, which wraps the actual import specifiers.
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

impl LanguageExtractor for TypeScriptExtractor {
    fn language(&self) -> Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }

    fn lang_name(&self) -> &str {
        "typescript"
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

        // Regular and abstract classes
        for (node, capture_name) in self.execute_query(QUERY_CLASSES, root, source) {
            if capture_name != "class" {
                continue;
            }

            let name_node = match node.child_by_field_name("name") {
                Some(n) => n,
                None => continue,
            };
            let name = get_node_text(&name_node, source).to_string();

            // Find base classes and implemented interfaces via class_heritage
            let mut bases = Vec::new();
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "class_heritage" {
                        for j in 0..child.child_count() {
                            if let Some(clause) = child.child(j) {
                                match clause.kind() {
                                    "extends_clause" | "implements_clause" => {
                                        for k in 0..clause.child_count() {
                                            if let Some(sub) = clause.child(k) {
                                                if matches!(
                                                    sub.kind(),
                                                    "identifier"
                                                        | "type_identifier"
                                                        | "member_expression"
                                                ) {
                                                    bases.push(
                                                        get_node_text(&sub, source).to_string(),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    _ => {}
                                }
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

        // Interfaces stored as classes with "[interface]" prefix in name
        for (node, capture_name) in self.execute_query(QUERY_INTERFACES, root, source) {
            if capture_name != "interface_node" {
                continue;
            }

            let name_node = match node.child_by_field_name("name") {
                Some(n) => n,
                None => continue,
            };
            let name = format!("[interface] {}", get_node_text(&name_node, source));

            // Interfaces can extend other interfaces
            let mut bases = Vec::new();
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "extends_type_clause" || child.kind() == "extends_clause" {
                        for j in 0..child.child_count() {
                            if let Some(sub) = child.child(j) {
                                if matches!(
                                    sub.kind(),
                                    "identifier" | "type_identifier" | "member_expression"
                                ) {
                                    bases.push(get_node_text(&sub, source).to_string());
                                }
                            }
                        }
                    }
                }
            }

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

        // Type aliases stored as classes with "[type]" prefix in name
        for (node, capture_name) in self.execute_query(QUERY_TYPE_ALIASES, root, source) {
            if capture_name != "type_alias_node" {
                continue;
            }

            let name_node = match node.child_by_field_name("name") {
                Some(n) => n,
                None => continue,
            };
            let name = format!("[type] {}", get_node_text(&name_node, source));

            let mut class = ClassData {
                name,
                line_number: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                bases: Vec::new(),
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

                // Find the import_clause child by iterating (not a field in this grammar)
                let import_clause = (0..node.child_count())
                    .filter_map(|i| node.child(i))
                    .find(|c| c.kind() == "import_clause");

                match import_clause {
                    None => {
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
                        self.process_import_clause(&clause, &import_source, line_number, source, &mut imports);
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
            let class_ctx = get_parent_context(
                &node,
                source,
                &["class_declaration", "abstract_class_declaration"],
            );

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

            // TypeScript type annotations on variable declarators
            let type_annotation = var_node
                .child_by_field_name("type")
                .map(|t| get_node_text(&t, source).to_string());

            let (context, context_type, _) = get_parent_context(&node, source, FC_TYPES);
            let class_context = if matches!(
                context_type.as_deref(),
                Some("class_declaration") | Some("abstract_class_declaration")
            ) {
                context.clone()
            } else {
                None
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
        let lang: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions_typed_params() {
        let code = r#"
function greet(name: string, age: number): void {
    console.log(name);
}

const add = (a: number, b: number): number => a + b;
"#;
        let (tree, source) = parse_source(code);
        let ext = TypeScriptExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 2);
        let names: Vec<&str> = funcs.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"greet"));
        assert!(names.contains(&"add"));
        // Check that typed params are extracted correctly
        let greet = funcs.iter().find(|f| f.name == "greet").unwrap();
        assert_eq!(greet.args.len(), 2);
    }

    #[test]
    fn test_find_functions_optional_params() {
        let code = r#"
function fetch(url: string, options?: RequestInit): Promise<Response> {
    return window.fetch(url, options);
}
"#;
        let (tree, source) = parse_source(code);
        let ext = TypeScriptExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "fetch");
        assert_eq!(funcs[0].args.len(), 2);
    }

    #[test]
    fn test_find_classes_with_implements() {
        let code = r#"
interface Serializable {
    serialize(): string;
}

class Animal {
    name: string;
    constructor(name: string) {
        this.name = name;
    }
}

class Dog extends Animal implements Serializable {
    serialize(): string {
        return this.name;
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = TypeScriptExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        // Should include Animal, Dog, and [interface] Serializable
        assert!(classes.len() >= 3);
        let names: Vec<&str> = classes.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Animal"));
        assert!(names.contains(&"Dog"));
        assert!(names.iter().any(|n| n.contains("Serializable")));
    }

    #[test]
    fn test_find_interfaces_and_type_aliases() {
        let code = r#"
interface User {
    name: string;
    age: number;
}

type ID = string | number;

class UserService {
    getUser(id: ID): User {
        return { name: "test", age: 0 };
    }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = TypeScriptExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        let names: Vec<&str> = classes.iter().map(|c| c.name.as_str()).collect();
        assert!(names.iter().any(|n| n.contains("User") && n.contains("[interface]")));
        assert!(names.iter().any(|n| n.contains("ID") && n.contains("[type]")));
        assert!(names.contains(&"UserService"));
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
import React from 'react';
import { useState, useEffect } from 'react';
import * as path from 'path';
"#;
        let (tree, source) = parse_source(code);
        let ext = TypeScriptExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 4);
    }

    #[test]
    fn test_find_calls() {
        let code = r#"
function main(): void {
    console.log("hello");
    const x = new Date();
    fetch("/api");
}
"#;
        let (tree, source) = parse_source(code);
        let ext = TypeScriptExtractor;
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
const x: number = 10;
let name: string = "hello";
const greet = (): void => {};
"#;
        let (tree, source) = parse_source(code);
        let ext = TypeScriptExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        // greet should be skipped
        assert_eq!(vars.len(), 2);
        let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"x"));
        assert!(names.contains(&"name"));
    }

    #[test]
    fn test_pre_scan() {
        let code = r#"
class MyClass {}
function myFunc(): void {}
const handler = () => {};
interface MyInterface {}
type MyType = string;
"#;
        let (tree, source) = parse_source(code);
        let ext = TypeScriptExtractor;
        let names = ext.pre_scan_definitions(&tree.root_node(), &source);
        assert!(names.contains(&"MyClass".to_string()));
        assert!(names.contains(&"myFunc".to_string()));
        assert!(names.contains(&"handler".to_string()));
        assert!(names.contains(&"MyInterface".to_string()));
        assert!(names.contains(&"MyType".to_string()));
    }

    #[test]
    fn test_complexity() {
        let code = r#"
function complex(x: number): void {
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
        let ext = TypeScriptExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 1);
        assert!(funcs[0].cyclomatic_complexity >= 4);
    }
}
