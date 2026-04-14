/// Heuristic resolution of function calls into CALLS edge payloads (no DB I/O).

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Python builtins to skip during resolution.
const PYTHON_BUILTINS: &[&str] = &[
    "print", "len", "range", "int", "str", "float", "bool", "list", "dict",
    "set", "tuple", "type", "isinstance", "issubclass", "hasattr", "getattr",
    "setattr", "delattr", "super", "property", "classmethod", "staticmethod",
    "abs", "all", "any", "bin", "chr", "dir", "divmod", "enumerate", "eval",
    "exec", "filter", "format", "frozenset", "globals", "hash", "hex", "id",
    "input", "iter", "map", "max", "min", "next", "object", "oct", "open",
    "ord", "pow", "repr", "reversed", "round", "slice", "sorted", "sum",
    "vars", "zip", "callable", "compile", "complex", "breakpoint",
    "__import__", "memoryview", "bytearray", "bytes",
];

/// A resolved call edge.
#[derive(Debug, Clone)]
pub struct ResolvedCall {
    pub call_type: String, // "function" or "file"
    pub caller_name: Option<String>,
    pub caller_file_path: String,
    pub caller_line_number: Option<usize>,
    pub called_name: String,
    pub called_file_path: String,
    pub line_number: usize,
    pub args: Vec<String>,
    pub full_call_name: String,
}

/// Input call data (mirrors the dict from parsing).
#[derive(Debug, Clone)]
pub struct CallInput {
    pub name: String,
    pub full_name: String,
    pub line_number: usize,
    pub args: Vec<String>,
    pub inferred_obj_type: Option<String>,
    pub context_name: Option<String>,
    pub context_type: Option<String>,
    pub context_line: Option<usize>,
    pub class_context_name: Option<String>,
}

/// File data needed for resolution (minimal subset).
pub struct FileCallData {
    pub path: String,
    pub lang: String,
    pub function_names: HashSet<String>,
    pub class_names: HashSet<String>,
    pub local_imports: HashMap<String, String>, // alias/short_name -> full_import_name
    pub calls: Vec<CallInput>,
}

/// The 6-category output of call resolution.
#[derive(Debug, Default)]
pub struct CallGroups {
    pub fn_to_fn: Vec<ResolvedCall>,
    pub fn_to_cls: Vec<ResolvedCall>,
    pub cls_to_fn: Vec<ResolvedCall>,
    pub cls_to_cls: Vec<ResolvedCall>,
    pub file_to_fn: Vec<ResolvedCall>,
    pub file_to_cls: Vec<ResolvedCall>,
}

/// Resolve a single function call to its target.
pub fn resolve_function_call(
    call: &CallInput,
    caller_file_path: &str,
    local_names: &HashSet<String>,
    local_imports: &HashMap<String, String>,
    imports_map: &HashMap<String, Vec<String>>,
    skip_external: bool,
) -> Option<ResolvedCall> {
    let called_name = &call.name;

    // Skip Python builtins
    if PYTHON_BUILTINS.contains(&called_name.as_str()) {
        return None;
    }

    let full_call = &call.full_name;
    let base_obj = if full_call.contains('.') {
        Some(full_call.split('.').next().unwrap_or(""))
    } else {
        None
    };

    let is_chained = full_call.matches('.').count() > 1;
    let self_receivers = ["self", "this", "super", "super()", "cls", "@"];

    let lookup_name = if is_chained && base_obj.map_or(false, |b| self_receivers.contains(&b)) {
        called_name.as_str()
    } else {
        base_obj.unwrap_or(called_name.as_str())
    };

    let mut resolved_path: Option<String> = None;
    let mut is_unresolved_external = false;

    // 1. Self/this/super receivers
    if base_obj.map_or(false, |b| self_receivers.contains(&b)) && !is_chained {
        resolved_path = Some(caller_file_path.to_string());
    }
    // 2. Local definitions
    else if local_names.contains(lookup_name) {
        resolved_path = Some(caller_file_path.to_string());
    }
    // 3. Inferred object type
    else if let Some(ref obj_type) = call.inferred_obj_type {
        if let Some(paths) = imports_map.get(obj_type.as_str()) {
            if !paths.is_empty() {
                resolved_path = Some(paths[0].clone());
            }
        }
    }

    // 4. Lookup in imports_map
    if resolved_path.is_none() {
        if let Some(paths) = imports_map.get(lookup_name) {
            if paths.len() == 1 {
                resolved_path = Some(paths[0].clone());
            } else if paths.len() > 1 {
                // Try to disambiguate via local imports
                if let Some(full_import) = local_imports.get(lookup_name) {
                    if let Some(direct_paths) = imports_map.get(full_import.as_str()) {
                        if direct_paths.len() == 1 {
                            resolved_path = Some(direct_paths[0].clone());
                        }
                    }
                    if resolved_path.is_none() {
                        let import_path = full_import.replace('.', "/");
                        for p in paths {
                            if p.contains(&import_path) {
                                resolved_path = Some(p.clone());
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    if resolved_path.is_none() {
        is_unresolved_external = true;
    }

    // 5. Fallback: try called_name directly
    if resolved_path.is_none() {
        if local_names.contains(called_name.as_str()) {
            resolved_path = Some(caller_file_path.to_string());
            is_unresolved_external = false;
        } else if let Some(candidates) = imports_map.get(called_name.as_str()) {
            if !candidates.is_empty() {
                // Try matching via local imports
                let mut found = false;
                for p in candidates {
                    for imp_name in local_imports.values() {
                        if p.contains(&imp_name.replace('.', "/")) {
                            resolved_path = Some(p.clone());
                            is_unresolved_external = false;
                            found = true;
                            break;
                        }
                    }
                    if found {
                        break;
                    }
                }
                if resolved_path.is_none() {
                    resolved_path = Some(candidates[0].clone());
                }
            }
        } else {
            resolved_path = Some(caller_file_path.to_string());
        }
    }

    if skip_external && is_unresolved_external {
        return None;
    }

    let resolved_path = resolved_path.unwrap_or_else(|| caller_file_path.to_string());

    // Determine call type based on context
    if let (Some(name), Some(_), Some(line)) =
        (&call.context_name, &call.context_type, call.context_line)
    {
        Some(ResolvedCall {
            call_type: "function".to_string(),
            caller_name: Some(name.clone()),
            caller_file_path: caller_file_path.to_string(),
            caller_line_number: Some(line),
            called_name: called_name.clone(),
            called_file_path: resolved_path,
            line_number: call.line_number,
            args: call.args.clone(),
            full_call_name: call.full_name.clone(),
        })
    } else {
        Some(ResolvedCall {
            call_type: "file".to_string(),
            caller_name: None,
            caller_file_path: caller_file_path.to_string(),
            caller_line_number: None,
            called_name: called_name.clone(),
            called_file_path: resolved_path,
            line_number: call.line_number,
            args: call.args.clone(),
            full_call_name: call.full_name.clone(),
        })
    }
}

/// Language extension map for filtering imports_map by language.
fn lang_extensions(lang: &str) -> Option<&[&str]> {
    match lang {
        "python" => Some(&[".py", ".ipynb"]),
        "javascript" => Some(&[".js", ".jsx", ".mjs", ".cjs"]),
        "typescript" => Some(&[".ts", ".tsx"]),
        "go" => Some(&[".go"]),
        "java" => Some(&[".java"]),
        "cpp" => Some(&[".cpp", ".h", ".hpp", ".hh"]),
        "c" => Some(&[".c"]),
        "c_sharp" => Some(&[".cs"]),
        "rust" => Some(&[".rs"]),
        "kotlin" => Some(&[".kt"]),
        "scala" => Some(&[".scala", ".sc"]),
        "ruby" => Some(&[".rb"]),
        "swift" => Some(&[".swift"]),
        "php" => Some(&[".php"]),
        "dart" => Some(&[".dart"]),
        "perl" => Some(&[".pl", ".pm"]),
        "haskell" => Some(&[".hs"]),
        "elixir" => Some(&[".ex", ".exs"]),
        _ => None,
    }
}

/// Filter imports_map to only include paths matching the caller's language extensions.
fn filter_imports_by_lang<'a>(
    imports_map: &'a HashMap<String, Vec<String>>,
    lang: &str,
) -> HashMap<String, Vec<String>> {
    let exts = match lang_extensions(lang) {
        Some(e) => e,
        None => return imports_map.clone(),
    };

    let mut filtered = HashMap::new();
    for (name, paths) in imports_map {
        let same_lang: Vec<String> = paths
            .iter()
            .filter(|p| {
                let ext = Path::new(p)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| format!(".{e}"))
                    .unwrap_or_default();
                exts.contains(&ext.as_str())
            })
            .cloned()
            .collect();

        if !same_lang.is_empty() {
            filtered.insert(name.clone(), same_lang);
        } else if paths.iter().all(|p| Path::new(p).extension().is_none()) {
            filtered.insert(name.clone(), paths.clone());
        }
    }
    filtered
}

/// Build the 6-category call groups from all file data.
pub fn build_function_call_groups(
    all_files: &[FileCallData],
    imports_map: &HashMap<String, Vec<String>>,
    file_class_lookup: &HashMap<String, HashSet<String>>,
    skip_external: bool,
) -> CallGroups {
    let mut groups = CallGroups::default();

    // Cache filtered imports_map per language
    let mut lang_cache: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();

    for file_data in all_files {
        let caller_file_path = &file_data.path;
        let local_names: HashSet<&str> = file_data
            .function_names
            .iter()
            .chain(file_data.class_names.iter())
            .map(|s| s.as_str())
            .collect();
        let local_names_owned: HashSet<String> = local_names.iter().map(|s| s.to_string()).collect();

        let effective_map = if !file_data.lang.is_empty() {
            lang_cache
                .entry(file_data.lang.clone())
                .or_insert_with(|| filter_imports_by_lang(imports_map, &file_data.lang))
        } else {
            imports_map
        };

        for call in &file_data.calls {
            let resolved = match resolve_function_call(
                call,
                caller_file_path,
                &local_names_owned,
                &file_data.local_imports,
                effective_map,
                skip_external,
            ) {
                Some(r) => r,
                None => continue,
            };

            let called_path = &resolved.called_file_path;
            let called_is_class = file_class_lookup
                .get(called_path.as_str())
                .map_or(false, |classes| classes.contains(&resolved.called_name));

            match resolved.call_type.as_str() {
                "file" => {
                    if called_is_class {
                        groups.file_to_cls.push(resolved);
                    } else {
                        groups.file_to_fn.push(resolved);
                    }
                }
                _ => {
                    let caller_is_class = resolved
                        .caller_name
                        .as_ref()
                        .map_or(false, |n| file_data.class_names.contains(n));

                    match (caller_is_class, called_is_class) {
                        (true, true) => groups.cls_to_cls.push(resolved),
                        (true, false) => groups.cls_to_fn.push(resolved),
                        (false, true) => groups.fn_to_cls.push(resolved),
                        (false, false) => groups.fn_to_fn.push(resolved),
                    }
                }
            }
        }
    }

    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_self_call() {
        let call = CallInput {
            name: "method".to_string(),
            full_name: "self.method".to_string(),
            line_number: 10,
            args: vec![],
            inferred_obj_type: None,
            context_name: Some("caller_func".to_string()),
            context_type: Some("function_definition".to_string()),
            context_line: Some(5),
            class_context_name: None,
        };
        let local_names = HashSet::new();
        let local_imports = HashMap::new();
        let imports_map = HashMap::new();

        let result = resolve_function_call(
            &call,
            "/path/to/file.py",
            &local_names,
            &local_imports,
            &imports_map,
            false,
        );

        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.called_file_path, "/path/to/file.py");
        assert_eq!(r.call_type, "function");
    }

    #[test]
    fn test_resolve_imported_call() {
        let call = CallInput {
            name: "some_func".to_string(),
            full_name: "some_func".to_string(),
            line_number: 15,
            args: vec![],
            inferred_obj_type: None,
            context_name: Some("main".to_string()),
            context_type: Some("function_definition".to_string()),
            context_line: Some(1),
            class_context_name: None,
        };
        let local_names = HashSet::new();
        let local_imports = HashMap::new();
        let mut imports_map = HashMap::new();
        imports_map.insert(
            "some_func".to_string(),
            vec!["/other/module.py".to_string()],
        );

        let result = resolve_function_call(
            &call,
            "/path/to/file.py",
            &local_names,
            &local_imports,
            &imports_map,
            false,
        );

        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.called_file_path, "/other/module.py");
    }

    #[test]
    fn test_skip_builtin() {
        let call = CallInput {
            name: "print".to_string(),
            full_name: "print".to_string(),
            line_number: 1,
            args: vec![],
            inferred_obj_type: None,
            context_name: None,
            context_type: None,
            context_line: None,
            class_context_name: None,
        };
        let result = resolve_function_call(
            &call,
            "/file.py",
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            false,
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_build_call_groups() {
        let mut imports_map = HashMap::new();
        imports_map.insert("Helper".to_string(), vec!["/helper.py".to_string()]);

        let file = FileCallData {
            path: "/main.py".to_string(),
            lang: "python".to_string(),
            function_names: ["main"].iter().map(|s| s.to_string()).collect(),
            class_names: HashSet::new(),
            local_imports: HashMap::new(),
            calls: vec![CallInput {
                name: "Helper".to_string(),
                full_name: "Helper".to_string(),
                line_number: 5,
                args: vec![],
                inferred_obj_type: None,
                context_name: Some("main".to_string()),
                context_type: Some("function_definition".to_string()),
                context_line: Some(1),
                class_context_name: None,
            }],
        };

        let mut file_class_lookup = HashMap::new();
        file_class_lookup.insert(
            "/helper.py".to_string(),
            ["Helper"].iter().map(|s| s.to_string()).collect(),
        );

        let groups = build_function_call_groups(
            &[file],
            &imports_map,
            &file_class_lookup,
            false,
        );

        assert_eq!(groups.fn_to_cls.len(), 1);
        assert_eq!(groups.fn_to_cls[0].called_name, "Helper");
    }
}
