//! Type information shared by the TypeScript and TSX extractors: receiver
//! types for calls (`this.customerService.get()` → `CustomerService`),
//! receiver chains, method return types and class fields.

use std::collections::HashMap;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::get_node_text;
use crate::types::VariableData;

/// Node kinds that declare a class (own fields / parameter properties).
pub const TS_CLASS_KINDS: &[&str] = &["class_declaration", "abstract_class_declaration", "class"];

/// Node kinds that scope parameters and local variables.
pub const TS_CALLABLE_KINDS: &[&str] = &[
    "method_definition",
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "arrow_function",
];

/// Typed declarations: class fields, parameters and local variables.
const QUERY_TYPED: &str = r#"
    (public_field_definition name: (property_identifier) @name) @decl
    (required_parameter pattern: (identifier) @name) @decl
    (optional_parameter pattern: (identifier) @name) @decl
    (variable_declarator name: (identifier) @name) @decl
"#;

/// Types that carry no useful receiver information.
const NON_TYPES: &[&str] = &[
    "string", "number", "boolean", "any", "unknown", "void", "never", "object", "null",
    "undefined", "String", "Number", "Boolean", "Object", "Function", "Array",
];

fn child_of_kind<'t>(node: &Node<'t>, kind: &str) -> Option<Node<'t>> {
    (0..node.child_count()).filter_map(|i| node.child(i)).find(|c| c.kind() == kind)
}

/// Simple class name of a type node (`Foo`, `Foo<T>`, `ns.Foo`, `Foo | null`).
pub fn type_name(node: &Node, source: &[u8]) -> Option<String> {
    let node = if node.kind() == "type_annotation" { node.named_child(0)? } else { *node };
    let name = match node.kind() {
        "type_identifier" => get_node_text(&node, source).to_string(),
        "generic_type" => return type_name(&node.child_by_field_name("name").or_else(|| node.named_child(0))?, source),
        "nested_type_identifier" => {
            let t = get_node_text(&node, source);
            t.rsplit('.').next().unwrap_or(t).to_string()
        }
        // `Foo | null | undefined` → Foo
        "union_type" => {
            return (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .find_map(|c| type_name(&c, source));
        }
        "parenthesized_type" => return type_name(&node.named_child(0)?, source),
        _ => return None,
    };
    (!NON_TYPES.contains(&name.as_str()) && name.starts_with(|c: char| c.is_ascii_uppercase())).then_some(name)
}

/// Raw text of a declared type (kept with generics, for return types).
pub fn type_text(annotation: &Node, source: &[u8]) -> Option<String> {
    let node = if annotation.kind() == "type_annotation" { annotation.named_child(0)? } else { *annotation };
    Some(get_node_text(&node, source).split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Type produced by an initializer: `new Foo()`, `inject(Foo)`,
/// `inject<Foo>(TOKEN)`, `x as Foo`.
fn initializer_type(value: &Node, source: &[u8]) -> Option<String> {
    match value.kind() {
        "new_expression" => {
            let ctor = value.child_by_field_name("constructor")?;
            let t = get_node_text(&ctor, source);
            let t = t.rsplit('.').next().unwrap_or(t);
            t.starts_with(|c: char| c.is_ascii_uppercase()).then(|| t.to_string())
        }
        "call_expression" => {
            let f = value.child_by_field_name("function")?;
            if get_node_text(&f, source) != "inject" {
                return None;
            }
            if let Some(targs) = value.child_by_field_name("type_arguments") {
                if let Some(t) = targs.named_child(0).and_then(|t| type_name(&t, source)) {
                    return Some(t);
                }
            }
            let arg = value.child_by_field_name("arguments")?.named_child(0)?;
            let t = get_node_text(&arg, source);
            (arg.kind() == "identifier" && t.starts_with(|c: char| c.is_ascii_uppercase())).then(|| t.to_string())
        }
        "as_expression" | "satisfies_expression" => type_name(&value.named_child(1)?, source),
        "await_expression" | "parenthesized_expression" | "non_null_expression" => {
            initializer_type(&value.named_child(0)?, source)
        }
        _ => None,
    }
}

fn enclosing(node: &Node, kinds: &[&str]) -> Option<usize> {
    let mut curr = node.parent();
    while let Some(p) = curr {
        if kinds.contains(&p.kind()) {
            return Some(p.start_byte());
        }
        curr = p.parent();
    }
    None
}

/// Is this parameter a constructor parameter property (`private svc: Svc`)?
fn is_parameter_property(param: &Node) -> bool {
    child_of_kind(param, "accessibility_modifier").is_some()
        || (0..param.child_count()).filter_map(|i| param.child(i)).any(|c| c.kind() == "readonly")
}

fn in_constructor(param: &Node, source: &[u8]) -> bool {
    let mut curr = param.parent();
    while let Some(p) = curr {
        if p.kind() == "method_definition" {
            return p.child_by_field_name("name").map_or(false, |n| get_node_text(&n, source) == "constructor");
        }
        if TS_CALLABLE_KINDS.contains(&p.kind()) {
            return false;
        }
        curr = p.parent();
    }
    false
}

/// `(scope start byte, name) → type`. Class fields and constructor parameter
/// properties are scoped to the class; parameters and locals to the
/// enclosing callable.
pub fn collect_typed_names(lang: &Language, root: &Node, source: &[u8]) -> HashMap<(usize, String), String> {
    let mut map = HashMap::new();
    let Ok(query) = Query::new(lang, QUERY_TYPED) else { return map };
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root, source);
    while let Some(m) = matches.next() {
        let (mut decl, mut name) = (None, None);
        for cap in m.captures {
            match names[cap.index as usize] {
                "decl" => decl = Some(cap.node),
                "name" => name = Some(cap.node),
                _ => {}
            }
        }
        let (Some(decl), Some(name)) = (decl, name) else { continue };
        let ty = decl
            .child_by_field_name("type")
            .and_then(|t| type_name(&t, source))
            .or_else(|| decl.child_by_field_name("value").and_then(|v| initializer_type(&v, source)));
        let Some(ty) = ty else { continue };
        let class_scoped = decl.kind() == "public_field_definition"
            || (matches!(decl.kind(), "required_parameter" | "optional_parameter")
                && is_parameter_property(&decl)
                && in_constructor(&decl, source));
        let scope = if class_scoped {
            enclosing(&decl, TS_CLASS_KINDS)
        } else {
            enclosing(&decl, TS_CALLABLE_KINDS).or_else(|| enclosing(&decl, TS_CLASS_KINDS)).or(Some(0))
        };
        if let Some(scope) = scope {
            map.entry((scope, get_node_text(&name, source).to_string())).or_insert(ty);
        }
    }
    map
}

/// Type of variable `var` visible at `node`: innermost callable first, then
/// enclosing classes (fields), then module scope.
pub fn lookup_var(node: &Node, var: &str, typed: &HashMap<(usize, String), String>) -> Option<String> {
    let mut curr = node.parent();
    while let Some(p) = curr {
        if TS_CALLABLE_KINDS.contains(&p.kind()) || TS_CLASS_KINDS.contains(&p.kind()) {
            if let Some(t) = typed.get(&(p.start_byte(), var.to_string())) {
                return Some(t.clone());
            }
        }
        curr = p.parent();
    }
    typed.get(&(0, var.to_string())).cloned()
}

/// Type of field `field` of the class enclosing `node`.
pub fn lookup_field(node: &Node, field: &str, typed: &HashMap<(usize, String), String>) -> Option<String> {
    let mut curr = node.parent();
    while let Some(p) = curr {
        if TS_CLASS_KINDS.contains(&p.kind()) {
            if let Some(t) = typed.get(&(p.start_byte(), field.to_string())) {
                return Some(t.clone());
            }
        }
        curr = p.parent();
    }
    None
}

fn enclosing_class_name(node: &Node, source: &[u8]) -> Option<String> {
    let mut curr = node.parent();
    while let Some(p) = curr {
        if TS_CLASS_KINDS.contains(&p.kind()) {
            return p.child_by_field_name("name").map(|n| get_node_text(&n, source).to_string());
        }
        curr = p.parent();
    }
    None
}

/// Peel a receiver into its root and member path:
/// `this.repo.find(id).items` → (this, ["repo", "find()", "items"]).
fn split_receiver<'t>(node: Node<'t>, source: &[u8]) -> (Option<Node<'t>>, Vec<String>) {
    match node.kind() {
        "member_expression" => {
            let prop = node.child_by_field_name("property").map(|p| get_node_text(&p, source).to_string());
            let (root, mut chain) = match node.child_by_field_name("object") {
                Some(o) => split_receiver(o, source),
                None => (None, Vec::new()),
            };
            chain.push(prop.unwrap_or_default());
            (root, chain)
        }
        "call_expression" => {
            let Some(f) = node.child_by_field_name("function") else { return (Some(node), Vec::new()) };
            if f.kind() != "member_expression" {
                // plain function call as a root: `getRepo().find()`
                return (Some(node), Vec::new());
            }
            let (root, mut chain) = split_receiver(f, source);
            if let Some(last) = chain.last_mut() {
                last.push_str("()");
            }
            (root, chain)
        }
        "parenthesized_expression" | "non_null_expression" | "await_expression" => match node.named_child(0) {
            Some(inner) => split_receiver(inner, source),
            None => (Some(node), Vec::new()),
        },
        _ => (Some(node), Vec::new()),
    }
}

/// Receiver facts for a call: (full_name `recv.method`, inferred receiver
/// type, receiver chain). `call_node` is a call_expression/new_expression.
pub fn call_receiver(
    call_node: &Node,
    name: &str,
    source: &[u8],
    typed: &HashMap<(usize, String), String>,
) -> (String, Option<String>, Vec<String>) {
    let squash = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(func) = call_node.child_by_field_name("function").filter(|f| f.kind() == "member_expression") else {
        // plain `foo()` / `new Foo()`
        let callee = call_node
            .child_by_field_name("function")
            .or_else(|| call_node.child_by_field_name("constructor"))
            .map(|f| squash(get_node_text(&f, source)))
            .unwrap_or_else(|| name.to_string());
        return (callee, None, Vec::new());
    };
    let Some(object) = func.child_by_field_name("object") else {
        return (name.to_string(), None, Vec::new());
    };
    let full_name = format!("{}.{}", squash(get_node_text(&object, source)), name);
    let (root, chain) = split_receiver(object, source);
    let Some(root) = root else { return (full_name, None, Vec::new()) };

    // this.field.m(): the field's declared type, no chain
    if root.kind() == "this" && chain.len() == 1 && !chain[0].ends_with("()") {
        return (full_name, lookup_field(call_node, &chain[0], typed), Vec::new());
    }
    let root_type = match root.kind() {
        "this" => enclosing_class_name(call_node, source),
        "identifier" => {
            let id = get_node_text(&root, source);
            lookup_var(call_node, id, typed)
                .or_else(|| id.starts_with(|c: char| c.is_ascii_uppercase()).then(|| id.to_string()))
        }
        "new_expression" => initializer_type(&root, source),
        _ => None,
    };
    if chain.is_empty() {
        // x.m(): fall back to the variable name (resolver treats lower-case
        // names as untyped)
        let fallback = (root.kind() == "identifier").then(|| get_node_text(&root, source).to_string());
        return (full_name, root_type.or(fallback), Vec::new());
    }
    match root_type {
        Some(t) => (full_name, Some(t), chain),
        None => (full_name, None, Vec::new()),
    }
}

/// Declared return type of a function/method node.
pub fn return_type(func: &Node, source: &[u8]) -> Option<String> {
    func.child_by_field_name("return_type").and_then(|t| type_text(&t, source))
}

/// Class fields and constructor parameter properties as typed variables
/// (context == class_context == the class), so the resolver can follow
/// `this.a.b.m()` chains.
pub fn class_field_variables(lang: &Language, root: &Node, source: &[u8], lang_name: &str) -> Vec<VariableData> {
    let mut out = Vec::new();
    let Ok(query) = Query::new(lang, QUERY_TYPED) else { return out };
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root, source);
    while let Some(m) = matches.next() {
        let (mut decl, mut name) = (None, None);
        for cap in m.captures {
            match names[cap.index as usize] {
                "decl" => decl = Some(cap.node),
                "name" => name = Some(cap.node),
                _ => {}
            }
        }
        let (Some(decl), Some(name)) = (decl, name) else { continue };
        let is_field = decl.kind() == "public_field_definition"
            || (matches!(decl.kind(), "required_parameter" | "optional_parameter")
                && is_parameter_property(&decl)
                && in_constructor(&decl, source));
        if !is_field {
            continue;
        }
        let Some(class) = enclosing_class_name(&decl, source) else { continue };
        let ty = decl
            .child_by_field_name("type")
            .and_then(|t| type_text(&t, source))
            .or_else(|| decl.child_by_field_name("value").and_then(|v| initializer_type(&v, source)));
        out.push(VariableData {
            name: get_node_text(&name, source).to_string(),
            line_number: name.start_position().row + 1,
            value: None,
            type_annotation: ty,
            context: Some(class.clone()),
            class_context: Some(class),
            lang: lang_name.to_string(),
            is_dependency: false,
        });
    }
    out
}
