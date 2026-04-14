use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{get_node_text, LanguageExtractor};
use crate::types::*;

const COMPLEXITY_TYPES: &[&str] = &[
    // Elixir complexity nodes are typically call nodes with specific keywords.
    // Since tree-sitter-elixir represents case/cond/if/unless/with as `call` nodes,
    // we track them via the generic walk which checks node types.
    // These are AST node types in tree-sitter-elixir:
    "call",          // covers if, unless, case, cond, with (filtered by keyword in practice)
    "binary_operator", // covers && / || / and / or
];

/// Keywords that define modules/namespaces in Elixir.
const MODULE_KEYWORDS: &[&str] = &["defmodule", "defprotocol", "defimpl"];

/// Keywords that define functions in Elixir.
const FUNCTION_KEYWORDS: &[&str] = &[
    "def", "defp", "defmacro", "defmacrop", "defguard", "defguardp", "defdelegate",
];

/// Keywords that represent imports/dependencies in Elixir.
const IMPORT_KEYWORDS: &[&str] = &["use", "import", "alias", "require"];

/// Keywords to exclude from general call detection.
const ELIXIR_KEYWORDS: &[&str] = &[
    "defmodule", "defprotocol", "defimpl",
    "def", "defp", "defmacro", "defmacrop", "defguard", "defguardp", "defdelegate",
    "use", "import", "alias", "require",
    "quote", "unquote", "case", "cond", "if", "unless", "for", "with",
    "try", "receive", "raise", "reraise", "throw", "super",
];

/// Elixir uses a very different AST from most languages.
/// Almost everything is a `call` node with an `identifier` target.
/// We use recursive tree walking rather than S-expression queries because
/// the tree-sitter-elixir grammar's call structure makes complex queries fragile.
pub struct ElixirExtractor;

impl ElixirExtractor {
    #[allow(dead_code)]
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

    /// Get the keyword identifier of a `call` node (e.g., "def", "defmodule").
    fn call_keyword<'a>(&self, node: &Node<'a>, source: &[u8]) -> Option<String> {
        if node.kind() != "call" {
            return None;
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "identifier" {
                    return Some(get_node_text(&child, source).to_string());
                }
            }
        }
        None
    }

    /// Find the enclosing module name by walking up the tree.
    fn enclosing_module_name(&self, node: &Node, source: &[u8]) -> Option<String> {
        let mut curr = node.parent();
        while let Some(parent) = curr {
            if parent.kind() == "call" {
                if let Some(kw) = self.call_keyword(&parent, source) {
                    if MODULE_KEYWORDS.contains(&kw.as_str()) {
                        // Find the alias argument (module name)
                        for i in 0..parent.child_count() {
                            if let Some(child) = parent.child(i) {
                                if child.kind() == "arguments" {
                                    for j in 0..child.child_count() {
                                        if let Some(arg) = child.child(j) {
                                            if arg.kind() == "alias" {
                                                return Some(
                                                    get_node_text(&arg, source).to_string(),
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
            curr = parent.parent();
        }
        None
    }

    /// Get parent context (module or function).
    fn get_elixir_parent_context(
        &self,
        node: &Node,
        source: &[u8],
    ) -> (Option<String>, Option<String>, Option<usize>) {
        let mut curr = node.parent();
        while let Some(parent) = curr {
            if parent.kind() == "call" {
                if let Some(kw) = self.call_keyword(&parent, source) {
                    if MODULE_KEYWORDS.contains(&kw.as_str()) {
                        for i in 0..parent.child_count() {
                            if let Some(child) = parent.child(i) {
                                if child.kind() == "arguments" {
                                    for j in 0..child.child_count() {
                                        if let Some(arg) = child.child(j) {
                                            if arg.kind() == "alias" {
                                                return (
                                                    Some(
                                                        get_node_text(&arg, source)
                                                            .to_string(),
                                                    ),
                                                    Some("module".to_string()),
                                                    Some(parent.start_position().row + 1),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if FUNCTION_KEYWORDS.contains(&kw.as_str()) {
                        for i in 0..parent.child_count() {
                            if let Some(child) = parent.child(i) {
                                if child.kind() == "arguments" {
                                    for j in 0..child.child_count() {
                                        if let Some(arg) = child.child(j) {
                                            if arg.kind() == "call" {
                                                if let Some(target) =
                                                    arg.child_by_field_name("target")
                                                {
                                                    return (
                                                        Some(
                                                            get_node_text(&target, source)
                                                                .to_string(),
                                                        ),
                                                        Some("function".to_string()),
                                                        Some(
                                                            parent.start_position().row + 1,
                                                        ),
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
            }
            curr = parent.parent();
        }
        (None, None, None)
    }

    /// Recursively find function definitions.
    fn find_functions_recursive(
        &self,
        node: &Node,
        source: &[u8],
        index_source: bool,
        functions: &mut Vec<FunctionData>,
    ) {
        if node.kind() == "call" {
            if let Some(kw) = self.call_keyword(node, source) {
                if FUNCTION_KEYWORDS.contains(&kw.as_str()) {
                    let mut func_name = None;
                    let mut args = Vec::new();

                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "arguments" {
                                for j in 0..child.child_count() {
                                    if let Some(arg) = child.child(j) {
                                        if arg.kind() == "call" {
                                            if let Some(target) =
                                                arg.child_by_field_name("target")
                                            {
                                                func_name = Some(
                                                    get_node_text(&target, source)
                                                        .to_string(),
                                                );
                                            }
                                            // Extract arguments
                                            for k in 0..arg.child_count() {
                                                if let Some(ac) = arg.child(k) {
                                                    if ac.kind() == "arguments" {
                                                        for l in 0..ac.child_count() {
                                                            if let Some(a) = ac.child(l) {
                                                                let t = a.kind();
                                                                if t != ","
                                                                    && t != "("
                                                                    && t != ")"
                                                                {
                                                                    args.push(
                                                                        get_node_text(
                                                                            &a, source,
                                                                        )
                                                                        .to_string(),
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        } else if arg.kind() == "identifier"
                                            && func_name.is_none()
                                        {
                                            func_name = Some(
                                                get_node_text(&arg, source).to_string(),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if let Some(name) = func_name {
                        let module_name = self.enclosing_module_name(node, source);

                        let mut func = FunctionData {
                            name,
                            line_number: node.start_position().row + 1,
                            end_line: node.end_position().row + 1,
                            args,
                            cyclomatic_complexity: 1,
                            context: module_name.clone(),
                            context_type: if module_name.is_some() {
                                Some("module".to_string())
                            } else {
                                None
                            },
                            class_context: module_name,
                            decorators: Vec::new(),
                            lang: self.lang_name().to_string(),
                            is_dependency: false,
                            source: None,
                            docstring: None,
                        };

                        if index_source {
                            func.source =
                                Some(get_node_text(node, source).to_string());
                        }

                        functions.push(func);
                    }
                }
            }
        }

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.find_functions_recursive(&child, source, index_source, functions);
            }
        }
    }

    /// Recursively find module definitions (used as classes).
    fn find_modules_recursive(
        &self,
        node: &Node,
        source: &[u8],
        index_source: bool,
        classes: &mut Vec<ClassData>,
    ) {
        if node.kind() == "call" {
            if let Some(kw) = self.call_keyword(node, source) {
                if MODULE_KEYWORDS.contains(&kw.as_str()) {
                    let mut module_name = None;
                    let mut has_do_block = false;

                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "arguments" {
                                for j in 0..child.child_count() {
                                    if let Some(arg) = child.child(j) {
                                        if arg.kind() == "alias" {
                                            module_name = Some(
                                                get_node_text(&arg, source).to_string(),
                                            );
                                        }
                                    }
                                }
                            } else if child.kind() == "do_block" {
                                has_do_block = true;
                            }
                        }
                    }

                    if let Some(name) = module_name {
                        if has_do_block {
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
                                class.source =
                                    Some(get_node_text(node, source).to_string());
                            }

                            classes.push(class);
                        }
                    }
                }
            }
        }

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.find_modules_recursive(&child, source, index_source, classes);
            }
        }
    }

    /// Recursively find import-like statements (use, import, alias, require).
    fn find_imports_recursive(
        &self,
        node: &Node,
        source: &[u8],
        imports: &mut Vec<ImportData>,
    ) {
        if node.kind() == "call" {
            if let Some(kw) = self.call_keyword(node, source) {
                if IMPORT_KEYWORDS.contains(&kw.as_str()) {
                    let mut path = None;

                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "arguments" {
                                for j in 0..child.child_count() {
                                    if let Some(arg) = child.child(j) {
                                        if arg.kind() == "alias" {
                                            path = Some(
                                                get_node_text(&arg, source).to_string(),
                                            );
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if let Some(p) = path {
                        let alias = if kw == "alias" {
                            p.rsplit('.').next().map(|s| s.to_string())
                        } else {
                            None
                        };

                        imports.push(ImportData {
                            name: p.clone(),
                            full_import_name: format!("{} {}", kw, p),
                            line_number: node.start_position().row + 1,
                            alias,
                            context: (None, None),
                            lang: self.lang_name().to_string(),
                            is_dependency: false,
                        });
                    }
                }
            }
        }

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.find_imports_recursive(&child, source, imports);
            }
        }
    }

    /// Recursively find function calls (excluding definition keywords).
    fn find_calls_recursive(
        &self,
        node: &Node,
        source: &[u8],
        calls: &mut Vec<CallData>,
    ) {
        if node.kind() == "call" {
            let mut target_name = None;
            let mut receiver = None;
            let mut dot_name = None;
            let mut args = Vec::new();

            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "dot" {
                        // Module.function() style call
                        let left = child.child_by_field_name("left");
                        let right = child.child_by_field_name("right");
                        if let Some(l) = left {
                            receiver = Some(get_node_text(&l, source).to_string());
                        }
                        if let Some(r) = right {
                            dot_name = Some(get_node_text(&r, source).to_string());
                        }
                    } else if child.kind() == "identifier" && target_name.is_none() {
                        target_name = Some(get_node_text(&child, source).to_string());
                    } else if child.kind() == "arguments" {
                        for j in 0..child.child_count() {
                            if let Some(arg) = child.child(j) {
                                let t = arg.kind();
                                if t != "," && t != "(" && t != ")" {
                                    args.push(get_node_text(&arg, source).to_string());
                                }
                            }
                        }
                    }
                }
            }

            if let (Some(recv), Some(name)) = (receiver, dot_name) {
                let full_name = format!("{}.{}", recv, name);
                let (ctx_name, ctx_type, ctx_line) =
                    self.get_elixir_parent_context(node, source);

                calls.push(CallData {
                    name: name.clone(),
                    full_name,
                    line_number: node.start_position().row + 1,
                    args,
                    inferred_obj_type: Some(recv),
                    context: (ctx_name, ctx_type, ctx_line),
                    class_context: (
                        self.enclosing_module_name(node, source),
                        Some("module".to_string()),
                    ),
                    lang: self.lang_name().to_string(),
                    is_dependency: false,
                    is_indirect_call: false,
                });
            } else if let Some(name) = target_name {
                if !ELIXIR_KEYWORDS.contains(&name.as_str()) {
                    let (ctx_name, ctx_type, ctx_line) =
                        self.get_elixir_parent_context(node, source);

                    calls.push(CallData {
                        name: name.clone(),
                        full_name: name,
                        line_number: node.start_position().row + 1,
                        args,
                        inferred_obj_type: None,
                        context: (ctx_name, ctx_type, ctx_line),
                        class_context: (
                            self.enclosing_module_name(node, source),
                            Some("module".to_string()),
                        ),
                        lang: self.lang_name().to_string(),
                        is_dependency: false,
                        is_indirect_call: false,
                    });
                }
            }
        }

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.find_calls_recursive(&child, source, calls);
            }
        }
    }
}

impl LanguageExtractor for ElixirExtractor {
    fn language(&self) -> Language {
        tree_sitter_elixir::LANGUAGE.into()
    }

    fn lang_name(&self) -> &str {
        "elixir"
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
        self.find_functions_recursive(root, source, index_source, &mut functions);
        functions
    }

    fn find_classes(
        &self,
        root: &Node,
        source: &[u8],
        index_source: bool,
    ) -> Vec<ClassData> {
        let mut classes = Vec::new();
        self.find_modules_recursive(root, source, index_source, &mut classes);
        classes
    }

    fn find_imports(&self, root: &Node, source: &[u8]) -> Vec<ImportData> {
        let mut imports = Vec::new();
        self.find_imports_recursive(root, source, &mut imports);
        imports
    }

    fn find_calls(&self, root: &Node, source: &[u8]) -> Vec<CallData> {
        let mut calls = Vec::new();
        self.find_calls_recursive(root, source, &mut calls);
        calls
    }

    fn find_variables(&self, _root: &Node, _source: &[u8]) -> Vec<VariableData> {
        // Elixir uses pattern matching for assignment; variables are not
        // traditionally declared. Return empty for now, matching the Python parser.
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_source(code: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let mut parser = Parser::new();
        let lang: Language = tree_sitter_elixir::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let source = code.as_bytes().to_vec();
        let tree = parser.parse(&source, None).unwrap();
        (tree, source)
    }

    #[test]
    fn test_find_functions() {
        let code = r#"
defmodule Calculator do
  def add(a, b) do
    a + b
  end

  defp subtract(a, b) do
    a - b
  end
end
"#;
        let (tree, source) = parse_source(code);
        let ext = ElixirExtractor;
        let funcs = ext.find_functions(&tree.root_node(), &source, false);
        assert!(funcs.len() >= 2);
        assert_eq!(funcs[0].name, "add");
        assert_eq!(funcs[1].name, "subtract");
    }

    #[test]
    fn test_find_modules() {
        let code = r#"
defmodule MyApp.Calculator do
  def compute, do: :ok
end

defmodule MyApp.Logger do
  def log(msg), do: IO.puts(msg)
end
"#;
        let (tree, source) = parse_source(code);
        let ext = ElixirExtractor;
        let classes = ext.find_classes(&tree.root_node(), &source, false);
        assert!(classes.len() >= 2);
    }

    #[test]
    fn test_find_imports() {
        let code = r#"
defmodule MyApp do
  use GenServer
  import Enum
  alias MyApp.Helpers
  require Logger
end
"#;
        let (tree, source) = parse_source(code);
        let ext = ElixirExtractor;
        let imports = ext.find_imports(&tree.root_node(), &source);
        assert!(imports.len() >= 4);
    }

    #[test]
    fn test_find_calls() {
        let code = r#"
defmodule MyApp do
  def run do
    IO.puts("hello")
    Enum.map([1, 2, 3], &(&1 * 2))
  end
end
"#;
        let (tree, source) = parse_source(code);
        let ext = ElixirExtractor;
        let calls = ext.find_calls(&tree.root_node(), &source);
        // Should find IO.puts, Enum.map, and possibly others
        assert!(calls.len() >= 2);
    }
}
