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
    /// Resolution tier, same scheme as upstream CGC (`resolution/calls.py`):
    /// 1 self receiver, 2 same file / enclosing class hierarchy,
    /// 3 receiver type resolved exactly, 4 receiver type by short name or via
    /// bases, 5 unique repo-wide name, 6 explicit import, 7 import path
    /// substring, 8 first of several candidates, 9 unresolved (caller file).
    pub tier: u8,
    /// EXTRACTED / INFERRED / AMBIGUOUS, derived from `tier`.
    pub confidence: &'static str,
    /// Class declaring the called method, when resolved through a type.
    pub called_class: Option<String>,
    /// Static type the method was looked up on (receiver type, enclosing
    /// class, or `Type` in `Type.m()`); virtual dispatch can only reach its
    /// subtypes.
    pub dispatch_class: Option<String>,
}

/// Map a resolution tier to upstream's confidence label.
pub fn confidence_label(tier: u8, unresolved_external: bool) -> &'static str {
    if unresolved_external || tier >= 8 {
        "AMBIGUOUS"
    } else if matches!(tier, 1 | 2 | 5 | 6) {
        "EXTRACTED"
    } else {
        "INFERRED"
    }
}

/// Input call data (mirrors the dict from parsing).
#[derive(Debug, Clone)]
pub struct CallInput {
    pub name: String,
    pub full_name: String,
    pub line_number: usize,
    pub args: Vec<String>,
    pub inferred_obj_type: Option<String>,
    /// Member path from the typed receiver root to this call
    /// (`["findById()", "get()"]`), see `CallData::receiver_chain`.
    pub receiver_chain: Vec<String>,
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
    /// class name -> names of the methods it declares (this file only)
    pub class_methods: HashMap<String, HashSet<String>>,
    /// class name -> base type names (simple names)
    pub class_bases: HashMap<String, Vec<String>>,
    /// (class, method) -> declared return type
    pub method_returns: HashMap<(String, String), String>,
    /// (class, field) -> declared field type
    pub field_types: HashMap<(String, String), String>,
}

/// Repo-wide view of types, their methods and bases, used to resolve
/// `receiver.method()` to the class that actually declares `method`
/// (walking up the inheritance chain), like upstream's
/// `method_target_for_type`.
#[derive(Debug, Default)]
pub struct TypeIndex {
    /// simple type name -> files declaring it
    files: HashMap<String, Vec<String>>,
    /// (type, file) -> declared method names
    methods: HashMap<(String, String), HashSet<String>>,
    /// (type, file) -> base type names
    bases: HashMap<(String, String), Vec<String>>,
    /// simple base name -> (subtype, file) declaring it as a base
    subtypes: HashMap<String, Vec<(String, String)>>,
    /// (type, file, method) -> return type
    returns: HashMap<(String, String, String), String>,
    /// (type, file, field) -> field type
    fields: HashMap<(String, String, String), String>,
}

/// Outcome of following a receiver chain.
enum ChainType {
    /// The chain ends on a repo type: (type, declaring file)
    Repo(String, String),
    /// The chain ends on a type defined outside the repo
    External,
    /// Couldn't follow the chain
    Unknown,
}

/// `Optional<Pet>` → ("Optional", ["Pet"]); `a.b.Foo` → ("Foo", []).
fn split_generic(raw: &str) -> (String, Vec<String>) {
    let raw = raw.trim().trim_end_matches("[]");
    let (base, args) = match (raw.find('<'), raw.rfind('>')) {
        (Some(l), Some(r)) if r > l => (&raw[..l], Some(&raw[l + 1..r])),
        _ => (raw, None),
    };
    let base = base.trim().rsplit('.').next().unwrap_or("").to_string();
    let mut out = Vec::new();
    if let Some(args) = args {
        let (mut depth, mut cur) = (0i32, String::new());
        for ch in args.chars() {
            match ch {
                '<' => depth += 1,
                '>' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(cur.trim().to_string());
                    cur.clear();
                    continue;
                }
                _ => {}
            }
            cur.push(ch);
        }
        out.push(cur.trim().to_string());
    }
    (base, out)
}

/// Element type exposed by well-known JDK wrappers for a member access:
/// `Optional<T>.orElseThrow()` → T, `List<T>.get()` → T, `Map<K, V>.get()` → V.
fn unwrap_container(base: &str, args: &[String], member: &str) -> Option<String> {
    match (base, member) {
        ("Optional", "get()" | "orElse()" | "orElseThrow()" | "orElseGet()") => args.first().cloned(),
        ("List" | "ArrayList" | "LinkedList", "get()" | "getFirst()" | "getLast()") => args.first().cloned(),
        ("Map" | "HashMap" | "LinkedHashMap" | "TreeMap", "get()" | "getOrDefault()") => args.last().cloned(),
        _ => None,
    }
}

impl TypeIndex {
    pub fn build(all_files: &[FileCallData]) -> Self {
        let mut index = TypeIndex::default();
        for f in all_files {
            for class in &f.class_names {
                index.files.entry(class.clone()).or_default().push(f.path.clone());
                let key = (class.clone(), f.path.clone());
                if let Some(m) = f.class_methods.get(class) {
                    index.methods.insert(key.clone(), m.clone());
                }
                for ((c, m), ret) in &f.method_returns {
                    if c == class {
                        index.returns.insert((class.clone(), f.path.clone(), m.clone()), ret.clone());
                    }
                }
                for ((c, fld), ty) in &f.field_types {
                    if c == class {
                        index.fields.insert((class.clone(), f.path.clone(), fld.clone()), ty.clone());
                    }
                }
                if let Some(b) = f.class_bases.get(class) {
                    for base in b {
                        index
                            .subtypes
                            .entry(Self::simple(base).to_string())
                            .or_default()
                            .push((class.clone(), f.path.clone()));
                    }
                    index.bases.insert(key, b.clone());
                }
            }
        }
        index
    }

    fn simple(name: &str) -> &str {
        name.rsplit('.').next().unwrap_or(name)
    }

    /// Breadth-first search from `ty` (declared in `file`) up through its
    /// bases for the nearest type declaring `method`. Returns the declaring
    /// file and the depth (0 = `ty` itself). With `skip_self`, `ty` itself is
    /// not considered (for `super.method()`).
    pub fn find_method(&self, ty: &str, file: &str, method: &str, skip_self: bool) -> Option<(String, String, usize)> {
        let mut queue = std::collections::VecDeque::new();
        let mut seen = HashSet::new();
        queue.push_back((Self::simple(ty).to_string(), file.to_string(), 0usize));
        while let Some((t, f, depth)) = queue.pop_front() {
            if depth > 12 || !seen.insert((t.clone(), f.clone())) {
                continue;
            }
            let key = (t, f);
            if !(skip_self && depth == 0)
                && self.methods.get(&key).map_or(false, |m| m.contains(method))
            {
                return Some((key.0, key.1, depth));
            }
            for base in self.bases.get(&key).into_iter().flatten() {
                let b = Self::simple(base);
                for bf in self.files.get(b).into_iter().flatten() {
                    queue.push_back((b.to_string(), bf.clone(), depth + 1));
                }
            }
        }
        None
    }

    /// Subtypes (transitively) of `ty` that override `method`: the targets of
    /// a virtual call through `ty` (interface or base class).
    pub fn overriding_subtypes(&self, ty: &str, method: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut queue: std::collections::VecDeque<String> = [Self::simple(ty).to_string()].into();
        while let Some(t) = queue.pop_front() {
            for (sub, file) in self.subtypes.get(&t).into_iter().flatten() {
                if !seen.insert((sub.clone(), file.clone())) || seen.len() > 200 {
                    continue;
                }
                if self.methods.get(&(sub.clone(), file.clone())).map_or(false, |m| m.contains(method)) {
                    out.push((sub.clone(), file.clone()));
                }
                queue.push_back(sub.clone());
            }
        }
        out
    }

    /// `ty` (in `file`) and its ancestors, nearest first.
    fn ancestors(&self, ty: &str, file: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut queue: std::collections::VecDeque<(String, String)> =
            [(Self::simple(ty).to_string(), file.to_string())].into();
        while let Some(key) = queue.pop_front() {
            if out.contains(&key) || out.len() > 32 {
                continue;
            }
            for base in self.bases.get(&key).into_iter().flatten() {
                let b = Self::simple(base);
                for bf in self.files.get(b).into_iter().flatten() {
                    queue.push_back((b.to_string(), bf.clone()));
                }
            }
            out.push(key);
        }
        out
    }

    /// File declaring `ty`, preferring `near`'s file, then its directory.
    fn type_file(&self, ty: &str, near: &str) -> Option<String> {
        let files = self.files.get(ty)?;
        let dir = |p: &str| p.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        files
            .iter()
            .find(|f| f.as_str() == near)
            .or_else(|| files.iter().find(|f| dir(f) == dir(near)))
            .or_else(|| files.first())
            .cloned()
    }

    /// Type of `member` ("m()" or field "f") on `ty`, searching its ancestors.
    /// Undeclared accessors fall back to fields: record `r.x()` and Lombok
    /// `getX()` / `isX()` read field `x`. Returns (raw type, declaring file).
    fn member_type(&self, ty: &str, file: &str, member: &str) -> Option<(String, String)> {
        let (name, is_call) = match member.strip_suffix("()") {
            Some(n) => (n, true),
            None => (member, false),
        };
        let getter_field = || -> Option<String> {
            let rest = name.strip_prefix("get").or_else(|| name.strip_prefix("is"))?;
            let mut chars = rest.chars();
            let first = chars.next()?.to_ascii_lowercase();
            Some(std::iter::once(first).chain(chars).collect())
        };
        for (t, f) in self.ancestors(ty, file) {
            let field = |n: &str| self.fields.get(&(t.clone(), f.clone(), n.to_string())).cloned();
            if is_call {
                if self.methods.get(&(t.clone(), f.clone())).map_or(false, |m| m.contains(name)) {
                    return self.returns.get(&(t.clone(), f.clone(), name.to_string())).map(|r| (r.clone(), f));
                }
                if let Some(ft) = field(name).or_else(|| getter_field().and_then(|g| field(&g))) {
                    return Some((ft, f));
                }
            } else if let Some(ft) = field(name) {
                return Some((ft, f));
            }
        }
        None
    }

    /// Follow `chain` starting from type `root` (declared in `root_file`).
    fn follow_chain(&self, root: &str, root_file: &str, chain: &[String]) -> ChainType {
        let mut raw = root.to_string();
        let mut near = root_file.to_string();
        for member in chain {
            let (base, args) = split_generic(&raw);
            if let Some(inner) = unwrap_container(&base, &args, member) {
                raw = inner;
                continue;
            }
            let Some(file) = self.type_file(&base, &near) else { return ChainType::Unknown };
            match self.member_type(&base, &file, member) {
                Some((ty, declaring)) => {
                    raw = ty;
                    near = declaring;
                }
                None => return ChainType::Unknown,
            }
        }
        let (base, _) = split_generic(&raw);
        match self.type_file(&base, &near) {
            Some(file) => ChainType::Repo(base, file),
            // Short names (T, ID, E) are type parameters, not external types.
            None if base.len() > 2 && base.starts_with(|c: char| c.is_ascii_uppercase()) => ChainType::External,
            None => ChainType::Unknown,
        }
    }

    pub fn declares(&self, ty: &str, file: &str) -> bool {
        self.files.get(Self::simple(ty)).map_or(false, |fs| fs.iter().any(|f| f == file))
    }
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

/// Locate the file declaring type `ty`. Returns (file, exact): exact when
/// the type maps to a single file or an explicit import disambiguates it.
fn locate_type(
    ty: &str,
    local_imports: &HashMap<String, String>,
    imports_map: &HashMap<String, Vec<String>>,
) -> Option<(String, bool)> {
    let simple = ty.rsplit('.').next().unwrap_or(ty);
    let paths = imports_map.get(simple).filter(|p| !p.is_empty())?;
    if paths.len() == 1 {
        return Some((paths[0].clone(), true));
    }
    let full = local_imports.get(simple).map(|s| s.as_str()).unwrap_or(ty);
    if full.contains('.') {
        let import_path = full.replace('.', "/");
        if let Some(p) = paths.iter().find(|p| p.contains(&import_path)) {
            return Some((p.clone(), true));
        }
    }
    Some((paths[0].clone(), false))
}

/// Resolve a single function call to its target.
pub fn resolve_function_call(
    call: &CallInput,
    caller_file_path: &str,
    local_names: &HashSet<String>,
    local_imports: &HashMap<String, String>,
    imports_map: &HashMap<String, Vec<String>>,
    types: &TypeIndex,
    skip_external: bool,
) -> Option<ResolvedCall> {
    let called_name = &call.name;
    let full_call = &call.full_name;

    // Skip Python builtins: only unqualified calls from Python files.
    // `len(x)` is a builtin; `repo.all()` / `service.list()` — or any call
    // from another language — is a regular method call.
    let is_python = caller_file_path.ends_with(".py") || caller_file_path.ends_with(".pyi");
    if is_python && !full_call.contains('.') && PYTHON_BUILTINS.contains(&called_name.as_str()) {
        return None;
    }

    let base_obj = if full_call.contains('.') {
        Some(full_call.split('.').next().unwrap_or(""))
    } else {
        None
    };

    let is_chained = full_call.matches('.').count() > 1;
    let self_receivers = ["self", "this", "super", "super()", "cls", "@"];
    let is_self_receiver = base_obj.map_or(false, |b| self_receivers.contains(&b));

    let lookup_name = if is_chained && is_self_receiver {
        called_name.as_str()
    } else {
        base_obj.unwrap_or(called_name.as_str())
    };

    // `obj.method()` on a receiver other than self/this: the target lives on
    // obj's type, never on a same-named function of the caller's file.
    let qualified_other = base_obj.map_or(false, |b| !b.is_empty() && !self_receivers.contains(&b));
    // Receiver whose (capitalised) type is known but defined outside the repo,
    // e.g. `list.get()` with `List<Foo> list` — don't guess a target by name.
    let external_receiver_type = qualified_other
        && call.inferred_obj_type.as_deref().map_or(false, |t| {
            t.starts_with(|c: char| c.is_ascii_uppercase())
                && !imports_map.contains_key(t.rsplit('.').next().unwrap_or(t))
                && !local_names.contains(t)
        });
    let enclosing_class = call.class_context_name.as_deref();
    let own = || caller_file_path.to_string();

    let mut resolved: Option<(String, u8)> = None;
    let mut called_class: Option<String> = None;
    let mut dispatch_class: Option<String> = None;
    let mut is_unresolved_external = false;

    // 1. Self/this/super receivers
    if is_self_receiver && !is_chained {
        let is_super = matches!(base_obj, Some("super") | Some("super()"));
        if !is_super && local_names.contains(called_name.as_str()) {
            resolved = Some((own(), 1));
        } else if let Some(cls) = enclosing_class {
            // super.m() → a base class; this.m() not declared here → inherited
            if let Some((c, f, _)) = types.find_method(cls, caller_file_path, called_name, is_super) {
                called_class = Some(c);
                // super.m() is not virtual
                dispatch_class = (!is_super).then(|| cls.to_string());
                resolved = Some((f, if is_super { 4 } else { 2 }));
            }
        }
        if resolved.is_none() {
            resolved = Some((own(), 1));
        }
    }
    // 2. Local definitions
    else if local_names.contains(lookup_name) {
        resolved = Some((own(), 2));
        // Static/qualified call on a local type whose method is inherited
        if qualified_other && types.declares(lookup_name, caller_file_path) {
            if let Some((c, f, depth)) = types.find_method(lookup_name, caller_file_path, called_name, false) {
                called_class = Some(c);
                dispatch_class = Some(lookup_name.to_string());
                if depth > 0 {
                    resolved = Some((f, 4));
                }
            }
        }
    }
    // 3. Inferred receiver type: the class declaring the method, walking bases.
    // Extractors fall back to the receiver *variable* name when the type is
    // unknown (`var list = ...`); only capitalised names are treated as types.
    // The type describes the *first* receiver segment, so it only applies to
    // `recv.m()` and `this.field.m()`, not to chains like `a.b.m()` / `a.b().m()`
    // (those would need member/return types, which extractors don't emit yet).
    else if let Some(obj_type) = call
        .inferred_obj_type
        .as_ref()
        .filter(|t| t.rsplit('.').next().map_or(false, |s| s.starts_with(|c: char| c.is_ascii_uppercase())))
        .filter(|_| call.receiver_chain.is_empty())
        .filter(|_| !is_chained || (is_self_receiver && full_call.matches('.').count() == 2 && !full_call.contains('(')))
    {
        if let Some((type_file, exact)) = locate_type(obj_type, local_imports, imports_map) {
            let found = types.find_method(obj_type, &type_file, called_name, false);
            called_class = found.as_ref().map(|(c, _, _)| c.clone());
            dispatch_class = Some(obj_type.rsplit('.').next().unwrap_or(obj_type).to_string());
            resolved = Some(match found {
                Some((_, f, 0)) => (f, if exact { 3 } else { 4 }),
                Some((_, f, _)) => (f, 4),
                // Declared by a type outside the repo (e.g. JpaRepository.findById):
                // point at the receiver's type.
                None => (type_file, 4),
            });
        }
    }

    // 3c. Chained receiver `root.a().b.m()`: follow return/field types
    if resolved.is_none() && !call.receiver_chain.is_empty() {
        let root = call
            .inferred_obj_type
            .as_deref()
            .filter(|t| t.rsplit('.').next().map_or(false, |s| s.starts_with(|c: char| c.is_ascii_uppercase())));
        if let Some(root) = root {
            let root_simple = root.rsplit('.').next().unwrap_or(root);
            let root_file = locate_type(root, local_imports, imports_map)
                .map(|(f, _)| f)
                .unwrap_or_else(own);
            match types.follow_chain(root_simple, &root_file, &call.receiver_chain) {
                ChainType::Repo(ty, file) => {
                    let found = types.find_method(&ty, &file, called_name, false);
                    called_class = found.as_ref().map(|(c, _, _)| c.clone());
                    dispatch_class = Some(ty);
                    resolved = Some((found.map(|(_, f, _)| f).unwrap_or(file), 4));
                }
                // e.g. repo.findAll().stream(): target is outside the graph
                ChainType::External => return None,
                ChainType::Unknown => {}
            }
        }
    }

    // 3b. Unqualified call inherited from the enclosing class's bases
    if resolved.is_none() && base_obj.is_none() {
        if let Some(cls) = enclosing_class {
            if let Some((c, f, _)) = types.find_method(cls, caller_file_path, called_name, true) {
                called_class = Some(c);
                dispatch_class = Some(cls.to_string());
                resolved = Some((f, 2));
            }
        }
    }

    // 4. Lookup in imports_map. For `recv.m()` this is only meaningful when
    // `recv` names a module/type (imported, or capitalised), not a variable.
    let receiver_is_symbol = !qualified_other
        || local_imports.contains_key(lookup_name)
        || lookup_name.starts_with(|c: char| c.is_ascii_uppercase());
    if resolved.is_none() && receiver_is_symbol {
        if let Some(paths) = imports_map.get(lookup_name) {
            if paths.len() == 1 {
                resolved = Some((paths[0].clone(), 5));
            } else if paths.len() > 1 {
                // Try to disambiguate via local imports
                if let Some(full_import) = local_imports.get(lookup_name) {
                    if let Some(direct_paths) = imports_map.get(full_import.as_str()) {
                        if direct_paths.len() == 1 {
                            resolved = Some((direct_paths[0].clone(), 6));
                        }
                    }
                    if resolved.is_none() {
                        let import_path = full_import.replace('.', "/");
                        if let Some(p) = paths.iter().find(|p| p.contains(&import_path)) {
                            resolved = Some((p.clone(), 7));
                        }
                    }
                }
            }
        }
        // `Type.method()` where Type resolved to a file: prefer the class
        // that declares the method (it may be inherited).
        if qualified_other {
            if let Some((ref file, _)) = resolved {
                if types.declares(lookup_name, file) {
                    if let Some((c, f, depth)) = types.find_method(lookup_name, file, called_name, false) {
                        called_class = Some(c);
                        dispatch_class = Some(lookup_name.to_string());
                        if depth > 0 {
                            resolved = Some((f, 4));
                        }
                    }
                }
            }
        }
    }

    // 5. Fallback: try called_name directly
    if resolved.is_none() {
        is_unresolved_external = true;
        if qualified_other && (external_receiver_type || !imports_map.contains_key(called_name.as_str())) {
            // The receiver's type is known but lives outside the repo: the
            // target can't be in the graph (upstream: receiver_resolution_failed).
            if external_receiver_type {
                return None;
            }
            // Pointing it at the caller's file would bind it to a same-named
            // local function (false recursion), so drop it in that case.
            if local_names.contains(called_name.as_str()) {
                return None;
            }
            resolved = Some((own(), 9));
        } else if !qualified_other && local_names.contains(called_name.as_str()) {
            resolved = Some((own(), 2));
            is_unresolved_external = false;
        } else if let Some(all_candidates) = imports_map.get(called_name.as_str()) {
            // A receiver call can't target the caller's own namesake.
            let candidates: Vec<&String> = all_candidates
                .iter()
                .filter(|p| {
                    !(qualified_other
                        && p.as_str() == caller_file_path
                        && local_names.contains(called_name.as_str()))
                })
                .collect();
            if candidates.is_empty() && qualified_other && local_names.contains(called_name.as_str()) {
                return None;
            }
            // Try matching via local imports
            for p in &candidates {
                if local_imports.values().any(|imp| p.contains(&imp.replace('.', "/"))) {
                    resolved = Some(((*p).clone(), 7));
                    is_unresolved_external = false;
                    break;
                }
            }
            if resolved.is_none() {
                if let Some(first) = candidates.first() {
                    resolved = Some(((*first).clone(), if candidates.len() == 1 { 5 } else { 8 }));
                    is_unresolved_external = candidates.len() != 1;
                }
            }
        } else {
            resolved = Some((own(), 9));
        }
    }

    if skip_external && is_unresolved_external {
        return None;
    }

    let (resolved_path, tier) = resolved.unwrap_or_else(|| (own(), 9));
    let confidence = confidence_label(tier, is_unresolved_external);

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
            tier,
            confidence,
            called_class: called_class.clone(),
            dispatch_class: dispatch_class.clone(),
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
            tier,
            confidence,
            called_class: called_class.clone(),
            dispatch_class: dispatch_class.clone(),
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
    let types = TypeIndex::build(all_files);

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
            let first = match resolve_function_call(
                call,
                caller_file_path,
                &local_names_owned,
                &file_data.local_imports,
                effective_map,
                &types,
                skip_external,
            ) {
                Some(r) => r,
                None => continue,
            };

            // Virtual dispatch (upstream: Java interface fan-out): a call
            // resolved to `Iface.m` / `Base.m` also reaches every subtype
            // overriding `m` in the same language.
            let mut targets = Vec::new();
            if matches!(file_data.lang.as_str(), "java" | "kotlin" | "c_sharp" | "scala") {
                let base = &first;
                if let Some(cls) = base.dispatch_class.clone() {
                    let exts = lang_extensions(&file_data.lang);
                    for (sub, file) in types.overriding_subtypes(&cls, &base.called_name) {
                        if file == base.called_file_path
                            || !exts.map_or(true, |e| e.iter().any(|x| file.ends_with(x)))
                        {
                            continue;
                        }
                        let mut extra = base.clone();
                        extra.called_file_path = file;
                        extra.called_class = Some(sub);
                        extra.tier = 4;
                        extra.confidence = confidence_label(4, false);
                        targets.push(extra);
                    }
                }
            }
            targets.insert(0, first);

            for resolved in targets {

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
            receiver_chain: vec![],
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
            &TypeIndex::default(),
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
            receiver_chain: vec![],
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
            &TypeIndex::default(),
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
            receiver_chain: vec![],
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
            &TypeIndex::default(),
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
            class_methods: HashMap::new(),
            class_bases: HashMap::new(),
            method_returns: HashMap::new(),
            field_types: HashMap::new(),
            calls: vec![CallInput {
                name: "Helper".to_string(),
                full_name: "Helper".to_string(),
                line_number: 5,
                args: vec![],
                inferred_obj_type: None,
                receiver_chain: vec![],
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

    fn receiver_call(name: &str, full: &str, obj_type: Option<&str>) -> CallInput {
        CallInput {
            name: name.to_string(),
            full_name: full.to_string(),
            line_number: 10,
            args: vec![],
            inferred_obj_type: obj_type.map(|s| s.to_string()),
            receiver_chain: vec![],
            context_name: Some(name.to_string()),
            context_type: Some("method_declaration".to_string()),
            context_line: Some(9),
            class_context_name: None,
        }
    }

    #[test]
    fn test_receiver_call_resolves_via_inferred_type_not_local_namesake() {
        // Controller.upsert() { return service.upsert(req); }
        let call = receiver_call("upsert", "service.upsert", Some("PointService"));
        let local_names: HashSet<String> = ["upsert".to_string()].into();
        let mut imports_map = HashMap::new();
        imports_map.insert("PointService".to_string(), vec!["/repo/PointService.java".to_string()]);

        let r = resolve_function_call(
            &call, "/repo/PointController.java", &local_names, &HashMap::new(), &imports_map, &TypeIndex::default(), false,
        )
        .unwrap();
        assert_eq!(r.called_file_path, "/repo/PointService.java");
    }

    #[test]
    fn test_unresolved_receiver_call_is_not_bound_to_local_namesake() {
        // list.add(x) inside a local add(): must not become add → add.
        let call = receiver_call("add", "items.add", Some("List"));
        let local_names: HashSet<String> = ["add".to_string()].into();
        let r = resolve_function_call(
            &call, "/repo/Cart.java", &local_names, &HashMap::new(), &HashMap::new(), &TypeIndex::default(), false,
        );
        assert!(r.is_none());
    }

    #[test]
    fn test_this_receiver_still_resolves_locally() {
        let call = receiver_call("helper", "this.helper", None);
        let local_names: HashSet<String> = ["helper".to_string()].into();
        let r = resolve_function_call(
            &call, "/repo/A.java", &local_names, &HashMap::new(), &HashMap::new(), &TypeIndex::default(), false,
        )
        .unwrap();
        assert_eq!(r.called_file_path, "/repo/A.java");
    }

    #[test]
    fn test_receiver_call_candidates_skip_caller_namesake() {
        // BaseCrudService.findChangesSince() { getRepository().findChangesSince(..) }
        let call = receiver_call("findChangesSince", "getRepository().findChangesSince", None);
        let local_names: HashSet<String> = ["findChangesSince".to_string()].into();
        let mut imports_map = HashMap::new();
        imports_map.insert(
            "findChangesSince".to_string(),
            vec!["/repo/BaseCrudService.java".to_string(), "/repo/Repo.java".to_string()],
        );
        let r = resolve_function_call(
            &call, "/repo/BaseCrudService.java", &local_names, &HashMap::new(), &imports_map, &TypeIndex::default(), false,
        )
        .unwrap();
        assert_eq!(r.called_file_path, "/repo/Repo.java");
    }

    #[test]
    fn test_builtin_names_only_skipped_for_unqualified_python_calls() {
        let resolve = |name: &str, full: &str, file: &str| {
            let call = receiver_call(name, full, None);
            resolve_function_call(&call, file, &HashSet::new(), &HashMap::new(), &HashMap::new(), &TypeIndex::default(), false)
        };
        assert!(resolve("len", "len", "/repo/a.py").is_none());
        assert!(resolve("list", "repo.list", "/repo/a.py").is_some());
        assert!(resolve("list", "service.list", "/repo/A.java").is_some());
        assert!(resolve("all", "all", "/repo/a.ts").is_some());
    }

    fn file(path: &str, classes: &[(&str, &[&str], &[&str])], calls: Vec<CallInput>) -> FileCallData {
        FileCallData {
            path: path.to_string(),
            lang: "java".to_string(),
            function_names: classes.iter().flat_map(|(_, m, _)| m.iter().map(|s| s.to_string())).collect(),
            class_names: classes.iter().map(|(c, _, _)| c.to_string()).collect(),
            local_imports: HashMap::new(),
            calls,
            class_methods: classes
                .iter()
                .map(|(c, m, _)| (c.to_string(), m.iter().map(|s| s.to_string()).collect()))
                .collect(),
            class_bases: classes
                .iter()
                .map(|(c, _, b)| (c.to_string(), b.iter().map(|s| s.to_string()).collect()))
                .collect(),
            method_returns: HashMap::new(),
            field_types: HashMap::new(),
        }
    }

    fn in_class(mut c: CallInput, class: &str) -> CallInput {
        c.class_context_name = Some(class.to_string());
        c
    }

    fn hierarchy_fixture() -> (Vec<FileCallData>, HashMap<String, Vec<String>>) {
        let files = vec![
            file("/r/BaseCrudService.java", &[("BaseCrudService", &["save", "search"], &[])], vec![]),
            file("/r/PetService.java", &[("PetService", &["create", "save"], &["BaseCrudService"])], vec![]),
            file("/r/PetController.java", &[("PetController", &["list"], &[])], vec![]),
        ];
        let mut imports_map = HashMap::new();
        for (n, p) in [("BaseCrudService", "/r/BaseCrudService.java"), ("PetService", "/r/PetService.java"), ("PetController", "/r/PetController.java")] {
            imports_map.insert(n.to_string(), vec![p.to_string()]);
        }
        (files, imports_map)
    }

    #[test]
    fn test_receiver_method_resolved_through_bases() {
        let (files, imports_map) = hierarchy_fixture();
        let types = TypeIndex::build(&files);
        let local: HashSet<String> = ["list".to_string()].into();
        let resolve = |name: &str| {
            let call = receiver_call(name, &format!("petService.{name}"), Some("PetService"));
            resolve_function_call(&call, "/r/PetController.java", &local, &HashMap::new(), &imports_map, &types, false).unwrap()
        };
        // declared on PetService itself
        let r = resolve("create");
        assert_eq!((r.called_file_path.as_str(), r.tier, r.confidence), ("/r/PetService.java", 3, "INFERRED"));
        // inherited from BaseCrudService
        let r = resolve("search");
        assert_eq!((r.called_file_path.as_str(), r.tier), ("/r/BaseCrudService.java", 4));
        // overridden: most-derived wins
        assert_eq!(resolve("save").called_file_path, "/r/PetService.java");
        // declared outside the repo (e.g. JpaRepository): falls back to the type's file
        assert_eq!(resolve("findById").called_file_path, "/r/PetService.java");
    }

    #[test]
    fn test_super_and_inherited_unqualified_calls() {
        let (files, imports_map) = hierarchy_fixture();
        let types = TypeIndex::build(&files);
        let local: HashSet<String> = ["create".to_string(), "save".to_string()].into();
        let resolve = |full: &str, name: &str| {
            let call = in_class(receiver_call(name, full, None), "PetService");
            resolve_function_call(&call, "/r/PetService.java", &local, &HashMap::new(), &imports_map, &types, false).unwrap()
        };
        // super.save() skips the override in PetService
        let r = resolve("super.save", "save");
        assert_eq!((r.called_file_path.as_str(), r.tier), ("/r/BaseCrudService.java", 4));
        // this.save() is the local override
        assert_eq!(resolve("this.save", "save").called_file_path, "/r/PetService.java");
        // this.search() / search() are inherited
        assert_eq!(resolve("this.search", "search").called_file_path, "/r/BaseCrudService.java");
        let r = resolve("search", "search");
        assert_eq!((r.called_file_path.as_str(), r.tier, r.confidence), ("/r/BaseCrudService.java", 2, "EXTRACTED"));
    }

    #[test]
    fn test_external_receiver_dropped_and_unresolved_is_ambiguous() {
        let types = TypeIndex::default();
        // List is not a repo type → no edge at all
        let call = receiver_call("add", "items.add", Some("List"));
        assert!(resolve_function_call(&call, "/r/A.java", &HashSet::new(), &HashMap::new(), &HashMap::new(), &types, false).is_none());
        // unknown receiver → kept, but as an AMBIGUOUS tier-9 edge
        let call = receiver_call("run", "thing.run", Some("thing"));
        let r = resolve_function_call(&call, "/r/A.java", &HashSet::new(), &HashMap::new(), &HashMap::new(), &types, false).unwrap();
        assert_eq!((r.tier, r.confidence), (9, "AMBIGUOUS"));
    }

    #[test]
    fn test_untyped_receiver_variable_is_not_looked_up_as_a_type() {
        // `var list = new ArrayList<>(); list.add(x)` → inferred_obj_type is the
        // variable name; a repo method called `list` must not capture it.
        let mut imports_map = HashMap::new();
        imports_map.insert("list".to_string(), vec!["/r/BaseCrudController.java".to_string()]);
        let call = receiver_call("add", "list.add", Some("list"));
        let r = resolve_function_call(&call, "/r/A.java", &HashSet::new(), &HashMap::new(), &imports_map, &TypeIndex::default(), false).unwrap();
        assert_ne!(r.called_file_path, "/r/BaseCrudController.java");
        assert_eq!(r.confidence, "AMBIGUOUS");
    }

    #[test]
    fn test_receiver_type_not_applied_to_chained_calls() {
        let (files, imports_map) = hierarchy_fixture();
        let types = TypeIndex::build(&files);
        // petService.repo().search(): the type of petService says nothing about repo()'s result
        let call = receiver_call("search", "petService.repo().search", Some("PetService"));
        let r = resolve_function_call(&call, "/r/PetController.java", &HashSet::new(), &HashMap::new(), &imports_map, &types, false);
        assert!(r.map_or(true, |r| r.called_file_path != "/r/BaseCrudService.java"));
    }

    #[test]
    fn test_interface_call_fans_out_to_implementations() {
        let mut ctrl = file("/r/PetController.java", &[("PetController", &["list"], &[])], vec![]);
        ctrl.calls = vec![receiver_call("findAll", "petService.findAll", Some("PetService"))];
        ctrl.calls[0].context_name = Some("list".to_string());
        let files = vec![
            ctrl,
            file("/r/PetService.java", &[("PetService", &["findAll"], &[])], vec![]),
            file("/r/PetServiceImpl.java", &[("PetServiceImpl", &["findAll", "helper"], &["PetService"])], vec![]),
            file("/r/CachedPetService.java", &[("CachedPetService", &["other"], &["PetServiceImpl"])], vec![]),
        ];
        let mut imports_map = HashMap::new();
        for f in &files {
            for c in &f.class_names {
                imports_map.insert(c.clone(), vec![f.path.clone()]);
            }
        }
        let groups = build_function_call_groups(&files, &imports_map, &HashMap::new(), false);
        let mut targets: Vec<(String, u8)> = groups
            .fn_to_fn
            .iter()
            .map(|r| (r.called_file_path.clone(), r.tier))
            .collect();
        targets.sort();
        // interface method (tier 3) + the one implementation overriding it (tier 4);
        // CachedPetService doesn't override findAll, so no edge to it
        assert_eq!(targets, vec![("/r/PetService.java".to_string(), 3), ("/r/PetServiceImpl.java".to_string(), 4)]);
    }

    #[test]
    fn test_fan_out_starts_from_receiver_type_not_declaring_class() {
        // lot.isActive() with `StockLot lot`: isActive is inherited from
        // Entity; Pet (a sibling) overriding it must not be a target.
        let mut caller = file("/r/T.java", &[("T", &["run"], &[])], vec![]);
        caller.calls = vec![receiver_call("isActive", "lot.isActive", Some("StockLot"))];
        caller.calls[0].context_name = Some("run".to_string());
        let files = vec![
            caller,
            file("/r/Entity.java", &[("Entity", &["isActive"], &[])], vec![]),
            file("/r/StockLot.java", &[("StockLot", &["qty"], &["Entity"])], vec![]),
            file("/r/Pet.java", &[("Pet", &["isActive"], &["Entity"])], vec![]),
        ];
        let mut imports_map = HashMap::new();
        for f in &files {
            for c in &f.class_names {
                imports_map.insert(c.clone(), vec![f.path.clone()]);
            }
        }
        let groups = build_function_call_groups(&files, &imports_map, &HashMap::new(), false);
        let targets: Vec<&str> = groups.fn_to_fn.iter().map(|r| r.called_file_path.as_str()).collect();
        assert_eq!(targets, vec!["/r/Entity.java"]);
    }

    fn chain_call(name: &str, root: &str, chain: &[&str]) -> CallInput {
        let mut c = receiver_call(name, &format!("x.{}.{name}", chain.join(".")), Some(root));
        c.receiver_chain = chain.iter().map(|s| s.to_string()).collect();
        c
    }

    #[test]
    fn test_chained_receivers_follow_return_and_field_types() {
        let mut repo = file("/r/PetRepository.java", &[("PetRepository", &["findById", "findAll"], &[])], vec![]);
        repo.method_returns.insert(("PetRepository".into(), "findById".into()), "Optional<Pet>".into());
        repo.method_returns.insert(("PetRepository".into(), "findAll".into()), "List<Pet>".into());
        let mut pet = file("/r/Pet.java", &[("Pet", &["vaccinate"], &[])], vec![]);
        pet.field_types.insert(("Pet".into(), "owner".into()), "Customer".into());
        let customer = file("/r/Customer.java", &[("Customer", &["notify"], &[])], vec![]);
        let files = vec![repo, pet, customer];
        let types = TypeIndex::build(&files);
        let mut imports_map = HashMap::new();
        for f in &files {
            for c in &f.class_names {
                imports_map.insert(c.clone(), vec![f.path.clone()]);
            }
        }
        let resolve = |c: CallInput| {
            resolve_function_call(&c, "/r/Svc.java", &HashSet::new(), &HashMap::new(), &imports_map, &types, false)
        };
        // repo.findById(id).orElseThrow().vaccinate()
        let r = resolve(chain_call("vaccinate", "PetRepository", &["findById()", "orElseThrow()"])).unwrap();
        assert_eq!((r.called_file_path.as_str(), r.tier), ("/r/Pet.java", 4));
        // repo.findAll().get(0).getOwner().notify()  — Lombok getter → field
        let r = resolve(chain_call("notify", "PetRepository", &["findAll()", "get()", "getOwner()"])).unwrap();
        assert_eq!(r.called_file_path, "/r/Customer.java");
        // pet.owner.notify() — field access
        let r = resolve(chain_call("notify", "Pet", &["owner"])).unwrap();
        assert_eq!(r.called_file_path, "/r/Customer.java");
        // repo.findAll().stream(): ends on an out-of-repo type → dropped
        assert!(resolve(chain_call("stream", "PetRepository", &["findAll()"])).is_none());
    }

    #[test]
    fn test_single_dot_chain_uses_chain_not_root_type() {
        // supplier("x").isActive() inside SupplierTest: the receiver is the
        // helper's return type, not SupplierTest.
        let mut t = file("/r/SupplierTest.java", &[("SupplierTest", &["supplier", "run"], &[])], vec![]);
        t.method_returns.insert(("SupplierTest".into(), "supplier".into()), "Supplier".into());
        let files = vec![t, file("/r/Supplier.java", &[("Supplier", &["isActive"], &[])], vec![])];
        let types = TypeIndex::build(&files);
        let mut imports_map = HashMap::new();
        imports_map.insert("Supplier".to_string(), vec!["/r/Supplier.java".to_string()]);
        imports_map.insert("SupplierTest".to_string(), vec!["/r/SupplierTest.java".to_string()]);
        let mut call = receiver_call("isActive", "supplier(\"x\").isActive", Some("SupplierTest"));
        call.receiver_chain = vec!["supplier()".to_string()];
        let local: HashSet<String> = ["supplier".to_string(), "run".to_string()].into();
        let r = resolve_function_call(&call, "/r/SupplierTest.java", &local, &HashMap::new(), &imports_map, &types, false).unwrap();
        assert_eq!(r.called_file_path, "/r/Supplier.java");
    }
}
