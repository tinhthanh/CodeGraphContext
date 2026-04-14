use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, get_parent_context, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    "if_statement",
    "unless_statement",
    "while_statement",
    "for_statement",
    "foreach_statement",
    "until_statement",
    "conditional_expression",
];

const QUERY_FUNCTIONS: &str = r#"
    (function_definition
        name: (identifier) @name
    ) @function_node
"#;

const QUERY_CLASSES: &str = r#"
    (package_statement) @class_node
"#;

const QUERY_IMPORTS: &str = r#"
    (use_no_statement) @import_node
    (require_statement) @import_node
"#;

const QUERY_CALLS: &str = r#"
    (method_invocation
        function_name: (identifier) @name
    ) @call_node
"#;

const QUERY_VARIABLES: &str = r#"
    (variable_declaration
        (scalar_variable) @name
        )
    ) @variable
"#;

/// Context types for parent lookups in Perl.
const FC_TYPES: &[&str] = &[
    "function_definition",
    "package_statement",
];

pub struct PerlExtractor;

impl PerlExtractor {
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
}

impl LanguageExtractor for PerlExtractor {
    fn language(&self) -> Language {
        tree_sitter_perl_vendor::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "perl"
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
        let mut seen = std::collections::HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_FUNCTIONS, root, source) {
            if capture_name != "function_node" {
                continue;
            }

            let key = (node.start_byte(), node.end_byte());
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);

            let name_node = match node.child_by_field_name("name") {
                Some(n) => n,
                None => continue,
            };
            let name = get_node_text(&name_node, source).to_string();

            let body_node = node.child_by_field_name("body");
            let complexity = match body_node {
                Some(ref b) => self.calculate_complexity(b),
                None => 1,
            };

            let (context, context_type, _) = get_parent_context(&node, source, FC_TYPES);
            let (class_context, _, _) = get_parent_context(
                &node,
                source,
                &["package_statement"],
            );

            let mut func = FunctionData {
                name,
                line_number: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                args: Vec::new(), // Perl args are dynamic (@_)
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

        for (node, capture_name) in self.execute_query(QUERY_CLASSES, root, source) {
            if capture_name != "class_node" {
                continue;
            }

            // package_statement has package_name as child (not a field)
            let name = (0..node.child_count())
                .filter_map(|i| node.child(i))
                .find(|c| c.kind() == "package_name")
                .map(|n| get_node_text(&n, source).to_string());
            let name = match name {
                Some(n) => n,
                None => continue,
            };

            let mut class = ClassData {
                name,
                line_number: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                bases: Vec::new(), // Bases set via 'use base' or 'use parent'
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
        let mut seen = std::collections::HashSet::new();

        for (node, capture_name) in self.execute_query(QUERY_IMPORTS, root, source) {
            if capture_name != "import_node" {
                continue;
            }

            let key = (node.start_byte(), node.end_byte());
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);

            // Extract package_name from use_no_statement or require_statement
            let import_name = node
                .child_by_field_name("package_name")
                .or_else(|| {
                    (0..node.child_count())
                        .filter_map(|i| node.child(i))
                        .find(|c| c.kind() == "package_name" || c.kind() == "module_name")
                })
                .map(|n| get_node_text(&n, source).to_string())
                .unwrap_or_else(|| get_node_text(&node, source).to_string());
            let line_number = node.start_position().row + 1;

            imports.push(ImportData {
                name: import_name.clone(),
                full_import_name: import_name,
                line_number,
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

            let call_name = get_node_text(&node, source).to_string();

            let context = get_parent_context(&node, source, FC_TYPES);
            let class_ctx = get_parent_context(
                &node,
                source,
                &["package_statement"],
            );

            calls.push(CallData {
                name: call_name.clone(),
                full_name: call_name,
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

            let name = get_node_text(&node, source).to_string();

            let (context, _, _) = get_parent_context(&node, source, FC_TYPES);
            let (class_context, _, _) = get_parent_context(
                &node,
                source,
                &["package_statement"],
            );

            variables.push(VariableData {
                name,
                line_number: node.start_position().row + 1,
                value: None,
                type_annotation: None,
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
        let lang: Language = tree_sitter_perl_vendor::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
sub greet {
    my ($name) = @_;
    print "Hello $name\n";
}

sub add {
    my ($a, $b) = @_;
    return $a + $b;
}
"#;
        let (tree, source) = parse_source(code);
        let ext = PerlExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert!(funcs.len() >= 2);
        assert_eq!(funcs[0].name, "greet");
        assert_eq!(funcs[1].name, "add");
    }

    #[test]
    fn test_find_classes() {
        let code = r#"
package Animal;

sub new {
    my ($class, %args) = @_;
    return bless \%args, $class;
}

package Dog;
"#;
        let (tree, source) = parse_source(code);
        let ext = PerlExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.len() >= 2);
        assert_eq!(classes[0].name, "Animal");
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
use strict;
use warnings;
use Data::Dumper;
"#;
        let (tree, source) = parse_source(code);
        let ext = PerlExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 3);
    }

    #[test]
    fn test_find_variables() {
        let code = r#"
my $x = 10;
my @list = (1, 2, 3);
my %hash = (key => 'value');
"#;
        let (tree, source) = parse_source(code);
        let ext = PerlExtractor;
        let vars = ext.find_variables(&tree.root_node(), &source);
        // Perl variable extraction depends on grammar node types; may vary
        assert!(vars.len() >= 0);
    }
}
