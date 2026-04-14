use std::collections::HashSet;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, get_parent_context, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    "if_expression",
    "for_statement",
    "while_statement",
    "when_expression",
    "catch_block",
    "elvis_expression",
];

const QUERY_FUNCTIONS: &str = r#"
    (function_declaration
        (simple_identifier) @name
    ) @function_node
"#;

const QUERY_CLASSES: &str = r#"
    (class_declaration
        (type_identifier) @name
    ) @class
"#;

const QUERY_INTERFACES: &str = r#"
    (class_declaration
        (type_identifier) @name
    ) @interface
"#;

const QUERY_OBJECTS: &str = r#"
    (object_declaration
        (type_identifier) @name
    ) @object
"#;

const QUERY_IMPORTS: &str = r#"
    (import_header) @import
"#;

const QUERY_CALLS: &str = r#"
    (call_expression) @call_node
"#;

const QUERY_VARIABLES: &str = r#"
    (property_declaration
        (variable_declaration
            (simple_identifier) @name
        )
    ) @variable
"#;

const QUERY_PRE_SCAN: &str = r#"
    (class_declaration (type_identifier) @name)
    (object_declaration (type_identifier) @name)
    (function_declaration (simple_identifier) @name)
"#;

pub struct KotlinExtractor;

impl KotlinExtractor {
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

    fn get_parent_context_kotlin(
        &self,
        node: &Node,
        source: &[u8],
    ) -> (Option<String>, Option<String>, Option<usize>) {
        let mut curr = node.parent();
        while let Some(parent) = curr {
            match parent.kind() {
                "function_declaration" => {
                    // Find simple_identifier child for function name
                    let mut name = None;
                    for i in 0..parent.child_count() {
                        if let Some(child) = parent.child(i) {
                            if child.kind() == "simple_identifier" {
                                name = Some(get_node_text(&child, source).to_string());
                                break;
                            }
                        }
                    }
                    return (
                        name,
                        Some(parent.kind().to_string()),
                        Some(parent.start_position().row + 1),
                    );
                }
                "class_declaration" | "object_declaration" => {
                    let mut name = None;
                    for i in 0..parent.child_count() {
                        if let Some(child) = parent.child(i) {
                            if child.kind() == "type_identifier"
                                || child.kind() == "simple_identifier"
                            {
                                name = Some(get_node_text(&child, source).to_string());
                                break;
                            }
                        }
                    }
                    return (
                        name,
                        Some(parent.kind().to_string()),
                        Some(parent.start_position().row + 1),
                    );
                }
                "companion_object" => {
                    let mut name = Some("Companion".to_string());
                    for i in 0..parent.child_count() {
                        if let Some(child) = parent.child(i) {
                            if child.kind() == "type_identifier"
                                || child.kind() == "simple_identifier"
                            {
                                name = Some(get_node_text(&child, source).to_string());
                                break;
                            }
                        }
                    }
                    return (
                        name,
                        Some("companion_object".to_string()),
                        Some(parent.start_position().row + 1),
                    );
                }
                _ => {}
            }
            curr = parent.parent();
        }
        (None, None, None)
    }

    fn get_class_context_kotlin(&self, node: &Node, source: &[u8]) -> Option<String> {
        let mut curr = node.parent();
        while let Some(parent) = curr {
            if parent.kind() == "class_declaration" || parent.kind() == "object_declaration"
            {
                for i in 0..parent.child_count() {
                    if let Some(child) = parent.child(i) {
                        if child.kind() == "type_identifier"
                            || child.kind() == "simple_identifier"
                        {
                            return Some(get_node_text(&child, source).to_string());
                        }
                    }
                }
                return None;
            }
            curr = parent.parent();
        }
        None
    }

    fn extract_kotlin_params(&self, func_node: &Node, source: &[u8]) -> Vec<String> {
        let mut params = Vec::new();

        for i in 0..func_node.child_count() {
            if let Some(child) = func_node.child(i) {
                if child.kind() == "function_value_parameters" {
                    // Parse the parameter text manually to handle nested generics
                    let params_text = get_node_text(&child, source);
                    let clean = params_text.trim();
                    let clean = if clean.starts_with('(') && clean.ends_with(')') {
                        &clean[1..clean.len() - 1]
                    } else {
                        clean
                    };

                    if clean.trim().is_empty() {
                        return params;
                    }

                    // Split by comma respecting angle bracket depth
                    let mut current = String::new();
                    let mut depth_angle = 0i32;
                    let mut depth_round = 0i32;

                    for ch in clean.chars() {
                        match ch {
                            '<' => depth_angle += 1,
                            '>' => depth_angle -= 1,
                            '(' => depth_round += 1,
                            ')' => depth_round -= 1,
                            ',' if depth_angle == 0 && depth_round == 0 => {
                                let trimmed = current.trim().to_string();
                                if !trimmed.is_empty() {
                                    // Extract parameter name (before colon)
                                    if let Some(colon_pos) = trimmed.find(':') {
                                        let lhs = trimmed[..colon_pos].trim();
                                        let tokens: Vec<&str> = lhs.split_whitespace().collect();
                                        if let Some(name) = tokens.last() {
                                            params.push(name.to_string());
                                        }
                                    } else {
                                        let tokens: Vec<&str> =
                                            trimmed.split_whitespace().collect();
                                        if let Some(name) = tokens.last() {
                                            params.push(name.to_string());
                                        }
                                    }
                                }
                                current.clear();
                                continue;
                            }
                            _ => {}
                        }
                        current.push(ch);
                    }

                    // Process the last parameter
                    let trimmed = current.trim().to_string();
                    if !trimmed.is_empty() {
                        if let Some(colon_pos) = trimmed.find(':') {
                            let lhs = trimmed[..colon_pos].trim();
                            let tokens: Vec<&str> = lhs.split_whitespace().collect();
                            if let Some(name) = tokens.last() {
                                params.push(name.to_string());
                            }
                        } else {
                            let tokens: Vec<&str> = trimmed.split_whitespace().collect();
                            if let Some(name) = tokens.last() {
                                params.push(name.to_string());
                            }
                        }
                    }

                    break;
                }
            }
        }

        params
    }
}

impl LanguageExtractor for KotlinExtractor {
    fn language(&self) -> Language {
        tree_sitter_kotlin::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "kotlin"
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
                Some(p) if p.kind() == "function_declaration" => p,
                _ => continue,
            };

            let node_id = (func_node.start_byte(), func_node.end_byte());
            if seen_nodes.contains(&node_id) {
                continue;
            }
            seen_nodes.insert(node_id);

            let name = get_node_text(&node, source).to_string();
            let args = self.extract_kotlin_params(&func_node, source);
            let complexity = self.calculate_complexity(&func_node);
            let (context, context_type, _) =
                self.get_parent_context_kotlin(&func_node, source);
            let class_context = self.get_class_context_kotlin(&func_node, source);

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
        let mut seen_nodes: HashSet<(usize, usize)> = HashSet::new();

        // Classes (including interfaces which are also class_declaration in Kotlin grammar)
        for (node, capture_name) in self.execute_query(QUERY_CLASSES, root, source) {
            if capture_name != "name" {
                continue;
            }
            let class_node = match node.parent() {
                Some(p) if p.kind() == "class_declaration" => p,
                _ => continue,
            };

            let node_id = (class_node.start_byte(), class_node.end_byte());
            if seen_nodes.contains(&node_id) {
                continue;
            }
            seen_nodes.insert(node_id);

            let name = get_node_text(&node, source).to_string();

            // Extract bases from delegation_specifier children
            let mut bases = Vec::new();
            for i in 0..class_node.child_count() {
                if let Some(child) = class_node.child(i) {
                    if child.kind() == "delegation_specifier" {
                        for j in 0..child.child_count() {
                            if let Some(spec) = child.child(j) {
                                if spec.kind() == "constructor_invocation" {
                                    for k in 0..spec.child_count() {
                                        if let Some(sub) = spec.child(k) {
                                            if sub.kind() == "user_type" {
                                                bases.push(
                                                    get_node_text(&sub, source)
                                                        .to_string(),
                                                );
                                                break;
                                            }
                                        }
                                    }
                                } else if spec.kind() == "user_type" {
                                    bases.push(get_node_text(&spec, source).to_string());
                                }
                            }
                        }
                    }
                }
            }

            let (context, _, _) = self.get_parent_context_kotlin(&class_node, source);

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

        // Object declarations
        for (node, capture_name) in self.execute_query(QUERY_OBJECTS, root, source) {
            if capture_name != "name" {
                continue;
            }
            let obj_node = match node.parent() {
                Some(p) if p.kind() == "object_declaration" => p,
                _ => continue,
            };

            let node_id = (obj_node.start_byte(), obj_node.end_byte());
            if seen_nodes.contains(&node_id) {
                continue;
            }
            seen_nodes.insert(node_id);

            let name = get_node_text(&node, source).to_string();

            let mut class = ClassData {
                name,
                line_number: obj_node.start_position().row + 1,
                end_line: obj_node.end_position().row + 1,
                bases: Vec::new(),
                context: None,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&obj_node, source).to_string());
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

            let text = get_node_text(&node, source).to_string();
            let path = text
                .trim_start_matches("import")
                .trim()
                .split(" as ")
                .next()
                .unwrap_or("")
                .trim()
                .to_string();

            let alias = if text.contains(" as ") {
                text.split(" as ").nth(1).map(|s| s.trim().to_string())
            } else {
                None
            };

            imports.push(ImportData {
                name: path.clone(),
                full_import_name: path,
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
        let mut seen: HashSet<(usize, usize)> = HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_CALLS, root, source) {
            if capture_name != "call_node" {
                continue;
            }

            let node_id = (node.start_byte(), node.end_byte());
            if seen.contains(&node_id) {
                continue;
            }
            seen.insert(node_id);

            // Determine function name from call_expression children
            let mut call_name = String::new();
            let mut base_obj: Option<String> = None;

            if let Some(first_child) = node.child(0) {
                match first_child.kind() {
                    "simple_identifier" => {
                        call_name = get_node_text(&first_child, source).to_string();
                    }
                    "navigation_expression" => {
                        // Extract operand and method name
                        let child_count = first_child.child_count();
                        if child_count >= 2 {
                            let operand = first_child.child(0);
                            let suffix = first_child.child(child_count - 1);

                            if let Some(op) = operand {
                                base_obj = Some(get_node_text(&op, source).to_string());
                            }

                            if let Some(suf) = suffix {
                                if suf.kind() == "navigation_suffix" {
                                    for k in 0..suf.child_count() {
                                        if let Some(c) = suf.child(k) {
                                            if c.kind() == "simple_identifier" {
                                                call_name =
                                                    get_node_text(&c, source).to_string();
                                                break;
                                            }
                                        }
                                    }
                                } else if suf.kind() == "simple_identifier" {
                                    call_name = get_node_text(&suf, source).to_string();
                                }
                            }
                        }
                    }
                    _ => {
                        // Fallback: use whole text
                        call_name = get_node_text(&first_child, source).to_string();
                    }
                }
            }

            if call_name.is_empty() {
                continue;
            }

            let full_name = match &base_obj {
                Some(obj) => format!("{}.{}", obj, call_name),
                None => call_name.clone(),
            };

            let (context_name, context_type, context_line) =
                self.get_parent_context_kotlin(&node, source);
            let class_context = self.get_class_context_kotlin(&node, source);

            calls.push(CallData {
                name: call_name,
                full_name,
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

        calls
    }

    fn find_variables(&self, root: &Node, source: &[u8]) -> Vec<VariableData> {
        let mut variables = Vec::new();

        for (node, capture_name) in self.execute_query(QUERY_VARIABLES, root, source) {
            if capture_name != "name" {
                continue;
            }

            let name = get_node_text(&node, source).to_string();

            // Walk up to property_declaration for value and type
            let mut prop_node = node.parent();
            while let Some(p) = prop_node {
                if p.kind() == "property_declaration" {
                    break;
                }
                prop_node = p.parent();
            }

            // Try to extract type from variable_declaration sibling
            let type_annotation = node.parent().and_then(|var_decl| {
                // In variable_declaration, look for user_type child
                for i in 0..var_decl.child_count() {
                    if let Some(child) = var_decl.child(i) {
                        if child.kind() == "user_type"
                            || child.kind() == "nullable_type"
                            || child.kind() == "function_type"
                        {
                            return Some(get_node_text(&child, source).to_string());
                        }
                    }
                }
                None
            });

            // Extract value from property_declaration (expression child after '=')
            let value = prop_node.and_then(|pn| {
                // Look for expression after '='
                let mut found_eq = false;
                for i in 0..pn.child_count() {
                    if let Some(child) = pn.child(i) {
                        if found_eq && child.kind() != "=" {
                            return Some(get_node_text(&child, source).to_string());
                        }
                        if get_node_text(&child, source) == "=" {
                            found_eq = true;
                        }
                    }
                }
                None
            });

            let (context, _, _) = self.get_parent_context_kotlin(&node, source);
            let class_context = self.get_class_context_kotlin(&node, source);

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
        let lang: Language = tree_sitter_kotlin::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
fun add(a: Int, b: Int): Int {
    return a + b
}

fun greet(name: String) {
    println("Hello $name")
}
"#;
        let (tree, source) = parse_source(code);
        let ext = KotlinExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 2);
        assert!(funcs.iter().any(|f| f.name == "add"));
        assert!(funcs.iter().any(|f| f.name == "greet"));
        let add = funcs.iter().find(|f| f.name == "add").unwrap();
        assert_eq!(add.args, vec!["a", "b"]);
    }

    #[test]
    fn test_find_classes() {
        let code = r#"
open class Animal(val name: String) {
    fun speak() {}
}

class Dog(name: String) : Animal(name) {
    fun bark() {}
}

object Singleton {
    fun instance() {}
}
"#;
        let (tree, source) = parse_source(code);
        let ext = KotlinExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.iter().any(|c| c.name == "Animal"));
        assert!(classes.iter().any(|c| c.name == "Dog"));
        assert!(classes.iter().any(|c| c.name == "Singleton"));
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
import kotlin.collections.ArrayList
import java.io.File as JFile
"#;
        let (tree, source) = parse_source(code);
        let ext = KotlinExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 2);
        assert!(imports
            .iter()
            .any(|i| i.name == "kotlin.collections.ArrayList"));
        let file_import = imports
            .iter()
            .find(|i| i.alias.as_deref() == Some("JFile"));
        assert!(file_import.is_some());
    }

    #[test]
    fn test_find_calls() {
        let code = r#"
fun main() {
    val x = add(1, 2)
    println("result: $x")
    listOf(1, 2, 3).forEach { print(it) }
}
"#;
        let (tree, source) = parse_source(code);
        let ext = KotlinExtractor;
        let calls = ext.find_calls(&tree.root_node(), &source);
        assert!(calls.iter().any(|c| c.name == "add" || c.name == "println"));
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
val x: Int = 10
var name = "Kotlin"
"#;
        let (tree, source) = parse_source(code);
        let ext = KotlinExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert!(vars.len() >= 2);
        assert!(vars.iter().any(|v| v.name == "x"));
        assert!(vars.iter().any(|v| v.name == "name"));
    }

    #[test]
    fn test_complexity() {
        let code = r#"
fun complex(x: Int): Int {
    if (x > 0) {
        for (i in 0..x) {
            when (i % 3) {
                0 -> println("fizz")
                1 -> println("buzz")
                else -> println(i)
            }
        }
    }
    return x
}
"#;
        let (tree, source) = parse_source(code);
        let ext = KotlinExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 1);
        // base 1 + if + for + when = at least 4
        assert!(funcs[0].cyclomatic_complexity >= 3);
    }
}
