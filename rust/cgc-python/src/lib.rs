use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

use std::collections::{HashMap, HashSet};
use std::path::Path;

mod conversions;

use cgc_core::parser;
use cgc_core::resolution::calls::{
    build_function_call_groups, CallGroups, CallInput, FileCallData, ResolvedCall,
};
use cgc_core::resolution::inheritance::{
    build_inheritance_and_csharp_files, ClassInfo, FileInheritanceData, InheritanceLink,
};

#[pyfunction]
#[pyo3(signature = (path, lang, is_dependency=false, index_source=false))]
fn parse_file(
    py: Python<'_>,
    path: &str,
    lang: &str,
    is_dependency: bool,
    index_source: bool,
) -> PyResult<PyObject> {
    let result = parser::parse_file(path, lang, is_dependency, index_source);
    conversions::parse_result_to_py(py, result)
}

#[pyfunction]
#[pyo3(signature = (file_specs, num_threads=None))]
fn parse_files_parallel(
    py: Python<'_>,
    file_specs: Vec<(String, String, bool)>,
    num_threads: Option<usize>,
) -> PyResult<Vec<PyObject>> {
    let results = py.allow_threads(|| parser::parse_files_parallel(&file_specs, num_threads));
    results
        .into_iter()
        .map(|r| conversions::parse_result_to_py(py, r))
        .collect()
}

#[pyfunction]
fn pre_scan_for_imports(py: Python<'_>, file_specs: Vec<(String, String)>) -> PyResult<PyObject> {
    let result = py.allow_threads(|| parser::pre_scan_for_imports(&file_specs));
    let dict = PyDict::new(py);
    for (name, paths) in result {
        let py_paths: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        dict.set_item(name, py_paths)?;
    }
    Ok(dict.into_any().unbind())
}

/// Parse files in parallel AND build imports_map in one pass.
/// Returns (list_of_file_data_dicts, imports_map_dict).
#[pyfunction]
#[pyo3(signature = (file_specs, num_threads=None))]
fn parse_and_prescan(
    py: Python<'_>,
    file_specs: Vec<(String, String, bool)>,
    num_threads: Option<usize>,
) -> PyResult<PyObject> {
    let (results, imports_map) = py.allow_threads(|| {
        parser::parse_and_prescan_parallel(&file_specs, num_threads)
    });

    let file_data_list: Vec<PyObject> = results
        .into_iter()
        .map(|r| conversions::parse_result_to_py(py, r))
        .collect::<PyResult<_>>()?;

    let py_imports = PyDict::new(py);
    for (name, paths) in imports_map {
        let py_paths: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        py_imports.set_item(name, py_paths)?;
    }

    let result = PyTuple::new(py, &[
        pyo3::types::PyList::new(py, &file_data_list)?.into_any().unbind(),
        py_imports.into_any().unbind(),
    ])?;
    Ok(result.into_any().unbind())
}

// --- Resolution bindings ---

/// Convert Python all_file_data + imports_map into Rust types, run resolution, return results.
#[pyfunction]
#[pyo3(signature = (all_file_data, imports_map, skip_external=false))]
fn resolve_call_groups(
    py: Python<'_>,
    all_file_data: &Bound<'_, PyList>,
    imports_map: &Bound<'_, PyDict>,
    skip_external: bool,
) -> PyResult<PyObject> {
    // Convert imports_map: Python dict -> Rust HashMap
    let rust_imports_map = py_dict_to_imports_map(imports_map)?;

    // Convert all_file_data: Python list of dicts -> Rust Vec<FileCallData>
    let mut rust_files = Vec::new();
    let mut file_class_lookup: HashMap<String, HashSet<String>> = HashMap::new();

    for item in all_file_data.iter() {
        let fd: &Bound<'_, PyDict> = item.downcast()?;
        let file_data = py_dict_to_file_call_data(fd)?;

        // Build file_class_lookup
        let resolved_path = std::fs::canonicalize(&file_data.path)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| file_data.path.clone());
        file_class_lookup.insert(resolved_path, file_data.class_names.clone());

        rust_files.push(file_data);
    }

    // Run resolution in Rust (release GIL)
    let groups = py.allow_threads(|| {
        build_function_call_groups(&rust_files, &rust_imports_map, &file_class_lookup, skip_external)
    });

    // Convert back to Python: 6-tuple of lists of dicts
    call_groups_to_py(py, &groups)
}

/// Resolve inheritance links.
#[pyfunction]
fn resolve_inheritance(
    py: Python<'_>,
    all_file_data: &Bound<'_, PyList>,
    imports_map: &Bound<'_, PyDict>,
) -> PyResult<PyObject> {
    let rust_imports_map = py_dict_to_imports_map(imports_map)?;

    let mut rust_files = Vec::new();
    for item in all_file_data.iter() {
        let fd: &Bound<'_, PyDict> = item.downcast()?;
        rust_files.push(py_dict_to_file_inheritance_data(fd)?);
    }

    let (batch, csharp_indices) = py.allow_threads(|| {
        build_inheritance_and_csharp_files(&rust_files, &rust_imports_map)
    });

    // Build result tuple: (inheritance_batch, csharp_files)
    let inheritance_list = PyList::empty(py);
    for link in &batch {
        let d = PyDict::new(py);
        d.set_item("child_name", &link.child_name)?;
        d.set_item("path", &link.path)?;
        d.set_item("parent_name", &link.parent_name)?;
        d.set_item("resolved_parent_file_path", &link.resolved_parent_file_path)?;
        inheritance_list.append(d)?;
    }

    // Collect C# file dicts from original list
    let csharp_list = PyList::empty(py);
    for &idx in &csharp_indices {
        let item = all_file_data.get_item(idx)?;
        csharp_list.append(item)?;
    }

    let result = PyTuple::new(py, &[
        inheritance_list.into_any().unbind(),
        csharp_list.into_any().unbind(),
    ])?;
    Ok(result.into_any().unbind())
}

/// Sanitize a dict of properties for graph DB storage.
#[pyfunction]
fn sanitize_props(py: Python<'_>, props: &Bound<'_, PyDict>) -> PyResult<PyObject> {
    let result = PyDict::new(py);
    for (key, value) in props.iter() {
        let key_str: String = key.extract()?;
        let sanitized = sanitize_py_value(py, &value)?;
        result.set_item(key_str, sanitized)?;
    }
    Ok(result.into_any().unbind())
}

// --- Helper conversion functions ---

fn py_dict_to_imports_map(d: &Bound<'_, PyDict>) -> PyResult<HashMap<String, Vec<String>>> {
    let mut map = HashMap::new();
    for (key, value) in d.iter() {
        let name: String = key.extract()?;
        let paths: Vec<String> = value.extract()?;
        map.insert(name, paths);
    }
    Ok(map)
}

fn py_dict_to_file_call_data(fd: &Bound<'_, PyDict>) -> PyResult<FileCallData> {
    let path: String = fd
        .get_item("path")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or_default();
    let lang: String = fd
        .get_item("lang")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or_default();

    // Resolve path
    let resolved_path = std::fs::canonicalize(&path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.clone());

    // Function names
    let mut function_names = HashSet::new();
    if let Some(funcs) = fd.get_item("functions")? {
        let funcs_list: &Bound<'_, PyList> = funcs.downcast()?;
        for f in funcs_list.iter() {
            let f_dict: &Bound<'_, PyDict> = f.downcast()?;
            if let Some(name) = f_dict.get_item("name")? {
                let n: String = name.extract()?;
                function_names.insert(n);
            }
        }
    }

    // Class names
    let mut class_names = HashSet::new();
    if let Some(classes) = fd.get_item("classes")? {
        let classes_list: &Bound<'_, PyList> = classes.downcast()?;
        for c in classes_list.iter() {
            let c_dict: &Bound<'_, PyDict> = c.downcast()?;
            if let Some(name) = c_dict.get_item("name")? {
                let n: String = name.extract()?;
                class_names.insert(n);
            }
        }
    }

    // Local imports: alias/short_name -> full_import_name
    let mut local_imports = HashMap::new();
    if let Some(imports) = fd.get_item("imports")? {
        let imports_list: &Bound<'_, PyList> = imports.downcast()?;
        for imp in imports_list.iter() {
            let imp_dict: &Bound<'_, PyDict> = imp.downcast()?;
            let name: String = imp_dict
                .get_item("name")?
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or_default();
            let alias: Option<String> = imp_dict
                .get_item("alias")?
                .and_then(|v| v.extract().ok());
            let key = alias.unwrap_or_else(|| {
                name.rsplit('.').next().unwrap_or(&name).to_string()
            });
            local_imports.insert(key, name);
        }
    }

    // Function calls
    let mut calls = Vec::new();
    if let Some(fc) = fd.get_item("function_calls")? {
        let fc_list: &Bound<'_, PyList> = fc.downcast()?;
        for call in fc_list.iter() {
            let c: &Bound<'_, PyDict> = call.downcast()?;
            calls.push(py_dict_to_call_input(c)?);
        }
    }

    Ok(FileCallData {
        path: resolved_path,
        lang,
        function_names,
        class_names,
        local_imports,
        calls,
    })
}

fn py_dict_to_call_input(c: &Bound<'_, PyDict>) -> PyResult<CallInput> {
    let name: String = c
        .get_item("name")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or_default();
    let full_name: String = c
        .get_item("full_name")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or_else(|| name.clone());
    let line_number: usize = c
        .get_item("line_number")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or(0);
    let args: Vec<String> = c
        .get_item("args")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or_default();
    let inferred_obj_type: Option<String> = c
        .get_item("inferred_obj_type")?
        .and_then(|v| v.extract().ok());

    // Context is a tuple (name, type, line) or None
    let (ctx_name, ctx_type, ctx_line) = if let Some(ctx) = c.get_item("context")? {
        if ctx.is_none() {
            (None, None, None)
        } else if let Ok(tuple) = ctx.downcast::<PyTuple>() {
            let n: Option<String> = tuple.get_item(0).ok().and_then(|v| v.extract().ok());
            let t: Option<String> = tuple.get_item(1).ok().and_then(|v| v.extract().ok());
            let l: Option<usize> = tuple.get_item(2).ok().and_then(|v| v.extract().ok());
            (n, t, l)
        } else {
            (None, None, None)
        }
    } else {
        (None, None, None)
    };

    let class_context_name: Option<String> = if let Some(cc) = c.get_item("class_context")? {
        if cc.is_none() {
            None
        } else if let Ok(tuple) = cc.downcast::<PyTuple>() {
            tuple.get_item(0).ok().and_then(|v| v.extract().ok())
        } else {
            cc.extract().ok()
        }
    } else {
        None
    };

    Ok(CallInput {
        name,
        full_name,
        line_number,
        args,
        inferred_obj_type,
        context_name: ctx_name,
        context_type: ctx_type,
        context_line: ctx_line,
        class_context_name,
    })
}

fn py_dict_to_file_inheritance_data(
    fd: &Bound<'_, PyDict>,
) -> PyResult<FileInheritanceData> {
    let path: String = fd
        .get_item("path")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or_default();
    let lang: String = fd
        .get_item("lang")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or_default();

    let resolved_path = std::fs::canonicalize(&path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path);

    let mut classes = Vec::new();
    if let Some(cls_list) = fd.get_item("classes")? {
        let cls_list: &Bound<'_, PyList> = cls_list.downcast()?;
        for c in cls_list.iter() {
            let c_dict: &Bound<'_, PyDict> = c.downcast()?;
            let name: String = c_dict
                .get_item("name")?
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or_default();
            let bases: Vec<String> = c_dict
                .get_item("bases")?
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or_default();
            classes.push(ClassInfo { name, bases });
        }
    }

    let mut local_imports = HashMap::new();
    if let Some(imports) = fd.get_item("imports")? {
        let imports_list: &Bound<'_, PyList> = imports.downcast()?;
        for imp in imports_list.iter() {
            let imp_dict: &Bound<'_, PyDict> = imp.downcast()?;
            let name: String = imp_dict
                .get_item("name")?
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or_default();
            let alias: Option<String> = imp_dict
                .get_item("alias")?
                .and_then(|v| v.extract().ok());
            let key = alias.unwrap_or_else(|| {
                name.rsplit('.').next().unwrap_or(&name).to_string()
            });
            local_imports.insert(key, name);
        }
    }

    Ok(FileInheritanceData {
        path: resolved_path,
        lang,
        classes,
        local_imports,
    })
}

fn resolved_call_to_py(py: Python<'_>, r: &ResolvedCall) -> PyResult<PyObject> {
    let d = PyDict::new(py);
    d.set_item("type", &r.call_type)?;
    if let Some(ref name) = r.caller_name {
        d.set_item("caller_name", name)?;
    }
    d.set_item("caller_file_path", &r.caller_file_path)?;
    if let Some(line) = r.caller_line_number {
        d.set_item("caller_line_number", line)?;
    }
    d.set_item("called_name", &r.called_name)?;
    d.set_item("called_file_path", &r.called_file_path)?;
    d.set_item("line_number", r.line_number)?;
    d.set_item("args", &r.args)?;
    d.set_item("full_call_name", &r.full_call_name)?;
    Ok(d.into_any().unbind())
}

fn call_groups_to_py(py: Python<'_>, groups: &CallGroups) -> PyResult<PyObject> {
    let to_list = |calls: &[ResolvedCall]| -> PyResult<PyObject> {
        let list = PyList::empty(py);
        for r in calls {
            list.append(resolved_call_to_py(py, r)?)?;
        }
        Ok(list.into_any().unbind())
    };

    let result = PyTuple::new(py, &[
        to_list(&groups.fn_to_fn)?,
        to_list(&groups.fn_to_cls)?,
        to_list(&groups.cls_to_fn)?,
        to_list(&groups.cls_to_cls)?,
        to_list(&groups.file_to_fn)?,
        to_list(&groups.file_to_cls)?,
    ])?;
    Ok(result.into_any().unbind())
}

const MAX_STR_LEN: usize = 4096;

fn sanitize_py_value(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    if value.is_none() {
        return Ok(py.None());
    }
    if let Ok(s) = value.extract::<String>() {
        if s.len() > MAX_STR_LEN {
            return Ok(s[..MAX_STR_LEN].into_pyobject(py)?.into_any().unbind());
        }
        return Ok(s.into_pyobject(py)?.into_any().unbind());
    }
    if let Ok(list) = value.downcast::<PyList>() {
        let new_list = PyList::empty(py);
        for item in list.iter() {
            new_list.append(sanitize_py_value(py, &item)?)?;
        }
        return Ok(new_list.into_any().unbind());
    }
    // For other types (int, float, bool), pass through
    Ok(value.clone().unbind())
}

#[pymodule]
fn _cgc_rust(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_file, m)?)?;
    m.add_function(wrap_pyfunction!(parse_files_parallel, m)?)?;
    m.add_function(wrap_pyfunction!(pre_scan_for_imports, m)?)?;
    m.add_function(wrap_pyfunction!(parse_and_prescan, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_call_groups, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_inheritance, m)?)?;
    m.add_function(wrap_pyfunction!(sanitize_props, m)?)?;
    Ok(())
}
