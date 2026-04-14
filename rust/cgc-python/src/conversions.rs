use pyo3::prelude::*;
use pyo3::types::PyDict;

use cgc_core::types::*;

/// Convert a ParseResult to a Python dict.
pub fn parse_result_to_py(py: Python<'_>, result: ParseResult) -> PyResult<PyObject> {
    match result {
        ParseResult::Ok(data) => file_data_to_py(py, &data),
        ParseResult::Err { path, error } => {
            let dict = PyDict::new(py);
            dict.set_item("path", path)?;
            dict.set_item("error", error)?;
            Ok(dict.into_any().unbind())
        }
    }
}

fn file_data_to_py(py: Python<'_>, data: &FileData) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("path", &data.path)?;
    dict.set_item("is_dependency", data.is_dependency)?;
    dict.set_item("lang", &data.lang)?;

    let functions: Vec<PyObject> = data
        .functions
        .iter()
        .map(|f| function_to_py(py, f))
        .collect::<PyResult<_>>()?;
    dict.set_item("functions", functions)?;

    let classes: Vec<PyObject> = data
        .classes
        .iter()
        .map(|c| class_to_py(py, c))
        .collect::<PyResult<_>>()?;
    dict.set_item("classes", classes)?;

    let variables: Vec<PyObject> = data
        .variables
        .iter()
        .map(|v| variable_to_py(py, v))
        .collect::<PyResult<_>>()?;
    dict.set_item("variables", variables)?;

    let imports: Vec<PyObject> = data
        .imports
        .iter()
        .map(|i| import_to_py(py, i))
        .collect::<PyResult<_>>()?;
    dict.set_item("imports", imports)?;

    let calls: Vec<PyObject> = data
        .function_calls
        .iter()
        .map(|c| call_to_py(py, c))
        .collect::<PyResult<_>>()?;
    dict.set_item("function_calls", calls)?;

    Ok(dict.into_any().unbind())
}

fn function_to_py(py: Python<'_>, f: &FunctionData) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("name", &f.name)?;
    dict.set_item("line_number", f.line_number)?;
    dict.set_item("end_line", f.end_line)?;
    dict.set_item("args", &f.args)?;
    dict.set_item("cyclomatic_complexity", f.cyclomatic_complexity)?;
    dict.set_item("context", &f.context)?;
    dict.set_item("context_type", &f.context_type)?;
    dict.set_item("class_context", &f.class_context)?;
    dict.set_item("decorators", &f.decorators)?;
    dict.set_item("lang", &f.lang)?;
    dict.set_item("is_dependency", f.is_dependency)?;

    if let Some(ref source) = f.source {
        dict.set_item("source", source)?;
    }
    if let Some(ref docstring) = f.docstring {
        dict.set_item("docstring", docstring)?;
    } else if f.source.is_some() {
        // If index_source is on but no docstring, set None
        dict.set_item("docstring", py.None())?;
    }

    Ok(dict.into_any().unbind())
}

fn class_to_py(py: Python<'_>, c: &ClassData) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("name", &c.name)?;
    dict.set_item("line_number", c.line_number)?;
    dict.set_item("end_line", c.end_line)?;
    dict.set_item("bases", &c.bases)?;
    dict.set_item("context", &c.context)?;
    dict.set_item("decorators", &c.decorators)?;
    dict.set_item("lang", &c.lang)?;
    dict.set_item("is_dependency", c.is_dependency)?;

    if let Some(ref source) = c.source {
        dict.set_item("source", source)?;
    }
    if let Some(ref docstring) = c.docstring {
        dict.set_item("docstring", docstring)?;
    } else if c.source.is_some() {
        dict.set_item("docstring", py.None())?;
    }

    Ok(dict.into_any().unbind())
}

fn variable_to_py(py: Python<'_>, v: &VariableData) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("name", &v.name)?;
    dict.set_item("line_number", v.line_number)?;
    dict.set_item("value", &v.value)?;
    dict.set_item("type", &v.type_annotation)?;
    dict.set_item("context", &v.context)?;
    dict.set_item("class_context", &v.class_context)?;
    dict.set_item("lang", &v.lang)?;
    dict.set_item("is_dependency", v.is_dependency)?;
    Ok(dict.into_any().unbind())
}

fn import_to_py(py: Python<'_>, i: &ImportData) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("name", &i.name)?;
    dict.set_item("full_import_name", &i.full_import_name)?;
    dict.set_item("line_number", i.line_number)?;
    dict.set_item("alias", &i.alias)?;
    // Context as tuple (name, type)
    let ctx = pyo3::types::PyTuple::new(py, &[
        i.context.0.as_deref().map_or_else(|| py.None(), |s| s.into_pyobject(py).unwrap().into_any().unbind()),
        i.context.1.as_deref().map_or_else(|| py.None(), |s| s.into_pyobject(py).unwrap().into_any().unbind()),
    ])?;
    dict.set_item("context", ctx)?;
    dict.set_item("lang", &i.lang)?;
    dict.set_item("is_dependency", i.is_dependency)?;
    Ok(dict.into_any().unbind())
}

fn call_to_py(py: Python<'_>, c: &CallData) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("name", &c.name)?;
    dict.set_item("full_name", &c.full_name)?;
    dict.set_item("line_number", c.line_number)?;
    dict.set_item("args", &c.args)?;
    dict.set_item("inferred_obj_type", &c.inferred_obj_type)?;

    // Context as tuple (name, type, line)
    let ctx = pyo3::types::PyTuple::new(py, &[
        c.context.0.as_deref().map_or_else(|| py.None(), |s| s.into_pyobject(py).unwrap().into_any().unbind()),
        c.context.1.as_deref().map_or_else(|| py.None(), |s| s.into_pyobject(py).unwrap().into_any().unbind()),
        c.context.2.map_or_else(|| py.None(), |n| n.into_pyobject(py).unwrap().into_any().unbind()),
    ])?;
    dict.set_item("context", ctx)?;

    // Class context as tuple (name, type)
    let class_ctx = pyo3::types::PyTuple::new(py, &[
        c.class_context.0.as_deref().map_or_else(|| py.None(), |s| s.into_pyobject(py).unwrap().into_any().unbind()),
        c.class_context.1.as_deref().map_or_else(|| py.None(), |s| s.into_pyobject(py).unwrap().into_any().unbind()),
    ])?;
    dict.set_item("class_context", class_ctx)?;

    dict.set_item("lang", &c.lang)?;
    dict.set_item("is_dependency", c.is_dependency)?;

    if c.is_indirect_call {
        dict.set_item("is_indirect_call", true)?;
    }

    Ok(dict.into_any().unbind())
}
