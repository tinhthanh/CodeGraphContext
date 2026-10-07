use std::collections::HashMap;
use std::fs;
use std::path::Path;

use tree_sitter::Parser;

use crate::lang::{get_extractor, get_extractor_by_ext};
use crate::types::*;

/// Parse a single file and return its data.
pub fn parse_file(
    path: &str,
    lang: &str,
    is_dependency: bool,
    index_source: bool,
) -> ParseResult {
    let extractor = match get_extractor(lang) {
        Some(e) => e,
        None => {
            return ParseResult::Err {
                path: path.to_string(),
                error: format!("Unsupported language: {lang}"),
            };
        }
    };

    let source = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            return ParseResult::Err {
                path: path.to_string(),
                error: format!("Failed to read file: {e}"),
            };
        }
    };

    let mut parser = Parser::new();
    parser
        .set_language(&extractor.language())
        .expect("Failed to set language");

    let tree = match parser.parse(&source, None) {
        Some(t) => t,
        None => {
            return ParseResult::Err {
                path: path.to_string(),
                error: "Failed to parse file".to_string(),
            };
        }
    };

    let root = tree.root_node();

    let functions = extractor.find_functions(&root, &source, index_source);
    let classes = extractor.find_classes(&root, &source, index_source);
    let imports = extractor.find_imports(&root, &source);
    let function_calls = extractor.find_calls(&root, &source);
    let variables = extractor.find_variables(&root, &source);
    let injections = extractor.find_injections(&root, &source);

    ParseResult::Ok(FileData {
        path: path.to_string(),
        functions,
        classes,
        variables,
        imports,
        function_calls,
        injections,
        is_dependency,
        lang: lang.to_string(),
    })
}

/// Parse multiple files in parallel using rayon.
pub fn parse_files_parallel(
    file_specs: &[(String, String, bool)], // (path, lang, is_dependency)
    num_threads: Option<usize>,
    index_source: bool,
) -> Vec<ParseResult> {
    use rayon::prelude::*;

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads.unwrap_or(0))
        .build()
        .expect("Failed to build rayon thread pool");

    pool.install(|| {
        file_specs
            .par_iter()
            .map(|(path, lang, is_dep)| parse_file(path, lang, *is_dep, index_source))
            .collect()
    })
}

/// Parse files in parallel AND build imports_map in one pass.
/// Eliminates the need for a separate pre-scan phase.
pub fn parse_and_prescan_parallel(
    file_specs: &[(String, String, bool)], // (path, lang, is_dependency)
    num_threads: Option<usize>,
    index_source: bool,
) -> (Vec<ParseResult>, HashMap<String, Vec<String>>) {
    use rayon::prelude::*;

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads.unwrap_or(0))
        .build()
        .expect("Failed to build rayon thread pool");

    // Parallel parse + extract names in one pass
    let results: Vec<(ParseResult, String, Vec<String>)> = pool.install(|| {
        file_specs
            .par_iter()
            .map(|(path, lang, is_dep)| {
                let result = parse_file(path, lang, *is_dep, index_source);

                // Extract pre-scan names from parsed data
                let resolved = fs::canonicalize(Path::new(path))
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| path.clone());

                let names = match &result {
                    ParseResult::Ok(data) => {
                        let mut names = Vec::new();
                        // Include ALL functions (not just top-level) to match Python pre-scan
                        for f in &data.functions {
                            names.push(f.name.clone());
                        }
                        for c in &data.classes {
                            names.push(c.name.clone());
                        }
                        names
                    }
                    ParseResult::Err { .. } => Vec::new(),
                };

                (result, resolved, names)
            })
            .collect()
    });

    // Build imports_map + collect parse results
    let mut parse_results = Vec::with_capacity(results.len());
    let mut imports_map: HashMap<String, Vec<String>> = HashMap::new();

    for (result, resolved_path, names) in results {
        for name in names {
            imports_map
                .entry(name)
                .or_default()
                .push(resolved_path.clone());
        }
        parse_results.push(result);
    }

    (parse_results, imports_map)
}

/// Pre-scan files to build imports_map: {symbol_name -> [file_paths]}.
pub fn pre_scan_for_imports(
    file_specs: &[(String, String)], // (path, extension)
) -> HashMap<String, Vec<String>> {
    use rayon::prelude::*;

    // Phase 1: Batch-resolve all paths upfront (reduce per-file syscalls)
    let resolved_paths: Vec<(String, String)> = file_specs
        .par_iter()
        .filter_map(|(path, ext)| {
            // Only process files with supported extensions
            if get_extractor_by_ext(ext).is_none() {
                return None;
            }
            let resolved = fs::canonicalize(Path::new(path))
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| path.clone());
            Some((path.clone(), resolved))
        })
        .collect();

    // Phase 2: Parallel parse + extract names (file I/O + tree-sitter)
    let results: Vec<(String, Vec<String>)> = file_specs
        .par_iter()
        .filter_map(|(path, ext)| {
            let extractor = get_extractor_by_ext(ext)?;
            let source = fs::read(path).ok()?;
            let mut parser = Parser::new();
            parser.set_language(&extractor.language()).ok()?;
            let tree = parser.parse(&source, None)?;
            let root = tree.root_node();
            let names = extractor.pre_scan_definitions(&root, &source);
            if names.is_empty() {
                return None;
            }
            Some((path.clone(), names))
        })
        .collect();

    // Phase 3: Build imports_map using pre-resolved paths
    let path_lookup: HashMap<&str, &str> = resolved_paths
        .iter()
        .map(|(orig, resolved)| (orig.as_str(), resolved.as_str()))
        .collect();

    let mut imports_map: HashMap<String, Vec<String>> = HashMap::new();
    for (path, names) in &results {
        let resolved = path_lookup
            .get(path.as_str())
            .copied()
            .unwrap_or(path.as_str());
        for name in names {
            imports_map
                .entry(name.clone())
                .or_default()
                .push(resolved.to_string());
        }
    }
    imports_map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: &str, ext: &str, code: &str) -> Vec<(String, String)> {
        let path = std::env::temp_dir().join(format!("cgc_kind_test_{lang}.{ext}"));
        fs::write(&path, code).unwrap();
        match parse_file(path.to_str().unwrap(), lang, false, false) {
            ParseResult::Ok(d) => d.classes.into_iter().map(|c| (c.name, c.kind)).collect(),
            ParseResult::Err { error, .. } => panic!("{error}"),
        }
    }

    fn has(v: &[(String, String)], name: &str, kind: &str) -> bool {
        v.iter().any(|(n, k)| n == name && k == kind)
    }

    #[test]
    fn test_class_kinds_across_languages() {
        let j = kinds("java", "java", "class A {} interface B {} enum C { X } record D(int x) {} @interface E {}");
        for (n, k) in [("A", "class"), ("B", "interface"), ("C", "enum"), ("D", "record"), ("E", "annotation")] {
            assert!(has(&j, n, k), "java {n} {k}: {j:?}");
        }
        let kt = kinds("kotlin", "kt", "class A\ninterface B\nenum class C { X }\nobject D\ndata class E(val x: Int)\n");
        for (n, k) in [("A", "class"), ("B", "interface"), ("C", "enum"), ("D", "object"), ("E", "class")] {
            assert!(has(&kt, n, k), "kotlin {n} {k}: {kt:?}");
        }
        let go = kinds("go", "go", "package p\ntype A struct{}\ntype B interface{ M() }\n");
        assert!(has(&go, "A", "struct") && has(&go, "B", "interface"), "go: {go:?}");
        let rs = kinds("rust", "rs", "struct A; enum B { X } trait C {}");
        assert!(has(&rs, "A", "struct") && has(&rs, "B", "enum") && has(&rs, "C", "trait"), "rust: {rs:?}");
        let ts = kinds("typescript", "ts", "class A {}\ninterface B {}\ntype C = string;\nenum D { X }\n");
        assert!(has(&ts, "A", "class") && has(&ts, "B", "interface") && has(&ts, "C", "type_alias"), "ts: {ts:?}");
        let sw = kinds("swift", "swift", "class A {}\nstruct B {}\nprotocol C {}\nenum D { case x }\n");
        assert!(has(&sw, "A", "class") && has(&sw, "B", "struct") && has(&sw, "C", "interface"), "swift: {sw:?}");
        let cs = kinds("c_sharp", "cs", "class A {} interface B {} struct C {} enum D { X } record E(int X);");
        assert!(has(&cs, "A", "class") && has(&cs, "B", "interface") && has(&cs, "C", "struct"), "c#: {cs:?}");
        let cpp = kinds("cpp", "cpp", "class A {}; struct B {};");
        assert!(has(&cpp, "A", "class") && has(&cpp, "B", "struct"), "cpp: {cpp:?}");
        let php = kinds("php", "php", "<?php\nclass A {}\ninterface B {}\ntrait C {}\n");
        assert!(has(&php, "A", "class") && has(&php, "B", "interface") && has(&php, "C", "trait"), "php: {php:?}");
        let sc = kinds("scala", "scala", "class A\ntrait B\nobject C\n");
        assert!(has(&sc, "A", "class") && has(&sc, "B", "trait") && has(&sc, "C", "object"), "scala: {sc:?}");
        let rb = kinds("ruby", "rb", "class A\nend\nmodule B\nend\n");
        assert!(has(&rb, "A", "class") && has(&rb, "B", "module"), "ruby: {rb:?}");
    }
}
