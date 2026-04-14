use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, get_parent_context, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    "if",
    "unless",
    "while",
    "until",
    "for",
    "case",
    "when",
    "rescue",
    "and",
    "or",
];

const QUERY_FUNCTIONS: &str = r#"
    (method
        name: (identifier) @name
    ) @function_node
"#;

const QUERY_CLASSES: &str = r#"
    (class
        name: (constant) @name
    ) @class
"#;

const QUERY_MODULES: &str = r#"
    (module
        name: (constant) @name
    ) @module
"#;

const QUERY_IMPORTS: &str = r#"
    (call
        method: (identifier) @method_name
        arguments: (argument_list
            (string) @path
        )
    ) @import
"#;

const QUERY_CALLS: &str = r#"
    (call
        method: (identifier) @name
    ) @call_node
"#;

const QUERY_VARIABLES: &str = r#"
    (assignment
        left: (identifier) @name
        right: (_) @value
    )
"#;

const QUERY_PRE_SCAN: &str = r#"
    (class name: (constant) @name)
    (module name: (constant) @name)
    (method name: (identifier) @name)
"#;

pub struct RubyExtractor;

impl RubyExtractor {
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

    fn get_parent_context_ruby(
        &self,
        node: &Node,
        source: &[u8],
    ) -> (Option<String>, Option<String>, Option<usize>) {
        let types = &["class", "module", "method"];
        get_parent_context(node, source, types)
    }
}

impl LanguageExtractor for RubyExtractor {
    fn language(&self) -> Language {
        tree_sitter_ruby::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "ruby"
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

        for (node, capture_name) in self.execute_query(QUERY_FUNCTIONS, root, source) {
            if capture_name != "name" {
                continue;
            }

            let method_node = match node.parent() {
                Some(p) if p.kind() == "method" => p,
                _ => continue,
            };

            let name = get_node_text(&node, source).to_string();

            // Parse parameters
            let mut args = Vec::new();
            for i in 0..method_node.child_count() {
                if let Some(child) = method_node.child(i) {
                    if child.kind() == "method_parameters" {
                        for j in 0..child.child_count() {
                            if let Some(param) = child.child(j) {
                                match param.kind() {
                                    "identifier" => {
                                        args.push(
                                            get_node_text(&param, source).to_string(),
                                        );
                                    }
                                    "optional_parameter" => {
                                        if let Some(name_node) =
                                            param.child_by_field_name("name")
                                        {
                                            args.push(
                                                get_node_text(&name_node, source)
                                                    .to_string(),
                                            );
                                        }
                                    }
                                    "splat_parameter" | "hash_splat_parameter"
                                    | "block_parameter" | "keyword_parameter" => {
                                        args.push(
                                            get_node_text(&param, source).to_string(),
                                        );
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }

            let complexity = self.calculate_complexity(&method_node);
            let (context, context_type, _) =
                self.get_parent_context_ruby(&method_node, source);
            let (class_context, _, _) =
                get_parent_context(&method_node, source, &["class", "module"]);

            let mut func = FunctionData {
                name,
                line_number: node.start_position().row + 1,
                end_line: method_node.end_position().row + 1,
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
                func.source = Some(get_node_text(&method_node, source).to_string());
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

        // Classes
        for (node, capture_name) in self.execute_query(QUERY_CLASSES, root, source) {
            if capture_name != "name" {
                continue;
            }

            let class_node = match node.parent() {
                Some(p) if p.kind() == "class" => p,
                _ => continue,
            };

            let name = get_node_text(&node, source).to_string();

            // Extract superclass
            let mut bases = Vec::new();
            if let Some(superclass) = class_node.child_by_field_name("superclass") {
                let base_text = get_node_text(&superclass, source).to_string();
                if !base_text.is_empty() {
                    bases.push(base_text);
                }
            }

            let (context, _, _) = get_parent_context(
                &class_node,
                source,
                &["class", "module"],
            );

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

        // Modules (as ClassData)
        for (node, capture_name) in self.execute_query(QUERY_MODULES, root, source) {
            if capture_name != "name" {
                continue;
            }

            let module_node = match node.parent() {
                Some(p) if p.kind() == "module" => p,
                _ => continue,
            };

            let name = get_node_text(&node, source).to_string();
            let (context, _, _) = get_parent_context(
                &module_node,
                source,
                &["class", "module"],
            );

            let mut class = ClassData {
                name,
                line_number: module_node.start_position().row + 1,
                end_line: module_node.end_position().row + 1,
                bases: Vec::new(),
                context,
                decorators: Vec::new(),
                lang: self.lang_name().to_string(),
                is_dependency: false,
                source: None,
                docstring: None,
            };

            if index_source {
                class.source = Some(get_node_text(&module_node, source).to_string());
            }

            classes.push(class);
        }

        classes
    }

    fn find_imports(&self, root: &Node, source: &[u8]) -> Vec<ImportData> {
        let mut imports = Vec::new();
        let captures = self.execute_query(QUERY_IMPORTS, root, source);

        // Group captures by import node
        let mut i = 0;
        while i < captures.len() {
            let (ref node, ref capture_name) = captures[i];

            if capture_name == "method_name" {
                let method_text = get_node_text(node, source);

                // Only process require/require_relative/load
                if method_text == "require"
                    || method_text == "require_relative"
                    || method_text == "load"
                {
                    // Look for the path capture next
                    if i + 1 < captures.len() && captures[i + 1].1 == "path" {
                        let path_node = &captures[i + 1].0;
                        let raw_path = get_node_text(path_node, source);
                        let path = raw_path.trim_matches('\'').trim_matches('"').to_string();

                        imports.push(ImportData {
                            name: path.clone(),
                            full_import_name: format!("{} '{}'", method_text, path),
                            line_number: node.start_position().row + 1,
                            alias: None,
                            context: (None, None),
                            lang: self.lang_name().to_string(),
                            is_dependency: false,
                        });
                    }
                }
            }

            i += 1;
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
                Some(p) if p.kind() == "call" => p,
                _ => continue,
            };

            let call_name = get_node_text(&node, source).to_string();

            // Skip require/require_relative (those are imports)
            if call_name == "require" || call_name == "require_relative" || call_name == "load"
            {
                continue;
            }

            // Get receiver for full_name
            let receiver = call_node
                .child_by_field_name("receiver")
                .map(|r| get_node_text(&r, source).to_string());

            let full_name = match &receiver {
                Some(r) => format!("{}.{}", r, call_name),
                None => call_name.clone(),
            };

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
                self.get_parent_context_ruby(&node, source);
            let (class_context, _, _) =
                get_parent_context(&node, source, &["class", "module"]);

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

        calls
    }

    fn find_variables(&self, root: &Node, source: &[u8]) -> Vec<VariableData> {
        let mut variables = Vec::new();

        for (node, capture_name) in self.execute_query(QUERY_VARIABLES, root, source) {
            if capture_name != "name" {
                continue;
            }

            let assignment_node = match node.parent() {
                Some(p) if p.kind() == "assignment" => p,
                _ => continue,
            };

            let name = get_node_text(&node, source).to_string();
            let value = assignment_node
                .child_by_field_name("right")
                .map(|v| get_node_text(&v, source).to_string());

            let (context, _, _) = self.get_parent_context_ruby(&node, source);
            let (class_context, _, _) =
                get_parent_context(&node, source, &["class", "module"]);

            variables.push(VariableData {
                name,
                line_number: node.start_position().row + 1,
                value,
                type_annotation: None, // Ruby is dynamically typed
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
        let lang: Language = tree_sitter_ruby::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
class Greeter
  def greet(name)
    puts "Hello #{name}"
  end

  def farewell
    puts "Goodbye"
  end
end
"#;
        let (tree, source) = parse_source(code);
        let ext = RubyExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert_eq!(funcs.len(), 2);
        assert!(funcs.iter().any(|f| f.name == "greet"));
        assert!(funcs.iter().any(|f| f.name == "farewell"));
    }

    #[test]
    fn test_find_classes_and_modules() {
        let code = r#"
module MyModule
  class Animal
    def speak
    end
  end

  class Dog < Animal
    def bark
    end
  end
end
"#;
        let (tree, source) = parse_source(code);
        let ext = RubyExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.iter().any(|c| c.name == "MyModule"));
        assert!(classes.iter().any(|c| c.name == "Animal"));
        assert!(classes.iter().any(|c| c.name == "Dog"));
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
require 'json'
require_relative 'helpers/utils'
"#;
        let (tree, source) = parse_source(code);
        let ext = RubyExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 2);
        assert!(imports.iter().any(|i| i.name == "json"));
        assert!(imports.iter().any(|i| i.name == "helpers/utils"));
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
x = 10
name = "Ruby"
"#;
        let (tree, source) = parse_source(code);
        let ext = RubyExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        assert!(vars.len() >= 2);
        assert!(vars.iter().any(|v| v.name == "x"));
        assert!(vars.iter().any(|v| v.name == "name"));
    }
}
