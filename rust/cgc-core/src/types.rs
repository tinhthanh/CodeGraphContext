/// Core data structures matching the Python dict schemas exactly.
/// These are the outputs of parsing and inputs to the Python persistence layer.

#[derive(Debug, Clone, Default)]
pub struct FileData {
    pub path: String,
    pub functions: Vec<FunctionData>,
    pub classes: Vec<ClassData>,
    pub variables: Vec<VariableData>,
    pub imports: Vec<ImportData>,
    pub function_calls: Vec<CallData>,
    pub is_dependency: bool,
    pub lang: String,
}

#[derive(Debug, Clone)]
pub struct FunctionData {
    pub name: String,
    pub line_number: usize,
    pub end_line: usize,
    pub args: Vec<String>,
    pub cyclomatic_complexity: usize,
    pub context: Option<String>,
    pub context_type: Option<String>,
    pub class_context: Option<String>,
    pub decorators: Vec<String>,
    pub lang: String,
    pub is_dependency: bool,
    pub source: Option<String>,
    pub docstring: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ClassData {
    pub name: String,
    pub line_number: usize,
    pub end_line: usize,
    pub bases: Vec<String>,
    pub context: Option<String>,
    pub decorators: Vec<String>,
    pub lang: String,
    pub is_dependency: bool,
    pub source: Option<String>,
    pub docstring: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VariableData {
    pub name: String,
    pub line_number: usize,
    pub value: Option<String>,
    pub type_annotation: Option<String>,
    pub context: Option<String>,
    pub class_context: Option<String>,
    pub lang: String,
    pub is_dependency: bool,
}

#[derive(Debug, Clone)]
pub struct ImportData {
    pub name: String,
    pub full_import_name: String,
    pub line_number: usize,
    pub alias: Option<String>,
    /// (context_name, context_type)
    pub context: (Option<String>, Option<String>),
    pub lang: String,
    pub is_dependency: bool,
}

#[derive(Debug, Clone)]
pub struct CallData {
    pub name: String,
    pub full_name: String,
    pub line_number: usize,
    pub args: Vec<String>,
    pub inferred_obj_type: Option<String>,
    /// (context_name, context_type, context_line)
    pub context: (Option<String>, Option<String>, Option<usize>),
    /// (class_name, class_type)
    pub class_context: (Option<String>, Option<String>),
    pub lang: String,
    pub is_dependency: bool,
    pub is_indirect_call: bool,
}

/// Result of parsing: either success with FileData or an error.
#[derive(Debug)]
pub enum ParseResult {
    Ok(FileData),
    Err { path: String, error: String },
}
