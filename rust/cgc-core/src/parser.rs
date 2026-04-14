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

    ParseResult::Ok(FileData {
        path: path.to_string(),
        functions,
        classes,
        variables,
        imports,
        function_calls,
        is_dependency,
        lang: lang.to_string(),
    })
}

/// Parse multiple files in parallel using rayon.
pub fn parse_files_parallel(
    file_specs: &[(String, String, bool)], // (path, lang, is_dependency)
    num_threads: Option<usize>,
) -> Vec<ParseResult> {
    use rayon::prelude::*;

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads.unwrap_or(0))
        .build()
        .expect("Failed to build rayon thread pool");

    pool.install(|| {
        file_specs
            .par_iter()
            .map(|(path, lang, is_dep)| parse_file(path, lang, *is_dep, false))
            .collect()
    })
}

/// Pre-scan files to build imports_map: {symbol_name -> [file_paths]}.
pub fn pre_scan_for_imports(
    file_specs: &[(String, String)], // (path, extension)
) -> HashMap<String, Vec<String>> {
    use rayon::prelude::*;

    // Group by extension for extractor reuse
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
            Some((path.clone(), names))
        })
        .collect();

    let mut imports_map: HashMap<String, Vec<String>> = HashMap::new();
    for (path, names) in results {
        let resolved = match fs::canonicalize(Path::new(&path)) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => path,
        };
        for name in names {
            imports_map.entry(name).or_default().push(resolved.clone());
        }
    }
    imports_map
}
