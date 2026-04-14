/// Resolve class inheritance into INHERITS row payloads (no DB I/O).

use std::collections::{HashMap, HashSet};

/// A resolved inheritance link.
#[derive(Debug, Clone)]
pub struct InheritanceLink {
    pub child_name: String,
    pub path: String,
    pub parent_name: String,
    pub resolved_parent_file_path: String,
}

/// File data needed for inheritance resolution (minimal subset).
pub struct FileInheritanceData {
    pub path: String,
    pub lang: String,
    pub classes: Vec<ClassInfo>,
    pub local_imports: HashMap<String, String>,
}

pub struct ClassInfo {
    pub name: String,
    pub bases: Vec<String>,
}

/// Resolve a single inheritance link.
pub fn resolve_inheritance_link(
    child_name: &str,
    base_class_str: &str,
    caller_file_path: &str,
    local_class_names: &HashSet<String>,
    local_imports: &HashMap<String, String>,
    imports_map: &HashMap<String, Vec<String>>,
) -> Option<InheritanceLink> {
    if base_class_str == "object" {
        return None;
    }

    let target_class_name = base_class_str.rsplit('.').next().unwrap_or(base_class_str);
    let mut resolved_path: Option<String> = None;

    if base_class_str.contains('.') {
        // Qualified name: package.ClassName
        let lookup_name = base_class_str.split('.').next().unwrap_or("");
        if let Some(full_import) = local_imports.get(lookup_name) {
            if let Some(paths) = imports_map.get(target_class_name) {
                let import_path = full_import.replace('.', "/");
                for p in paths {
                    if p.contains(&import_path) {
                        resolved_path = Some(p.clone());
                        break;
                    }
                }
            }
        }
    } else {
        // Simple name
        if local_class_names.contains(base_class_str) {
            resolved_path = Some(caller_file_path.to_string());
        } else if let Some(full_import) = local_imports.get(base_class_str) {
            if let Some(paths) = imports_map.get(target_class_name) {
                let import_path = full_import.replace('.', "/");
                for p in paths {
                    if p.contains(&import_path) {
                        resolved_path = Some(p.clone());
                        break;
                    }
                }
            }
        } else if let Some(paths) = imports_map.get(base_class_str) {
            if paths.len() == 1 {
                resolved_path = Some(paths[0].clone());
            }
        }
    }

    resolved_path.map(|rp| InheritanceLink {
        child_name: child_name.to_string(),
        path: caller_file_path.to_string(),
        parent_name: target_class_name.to_string(),
        resolved_parent_file_path: rp,
    })
}

/// Build inheritance batch and separate C# files.
/// Returns (inheritance_links, csharp_file_indices).
pub fn build_inheritance_and_csharp_files(
    all_files: &[FileInheritanceData],
    imports_map: &HashMap<String, Vec<String>>,
) -> (Vec<InheritanceLink>, Vec<usize>) {
    let mut inheritance_batch = Vec::new();
    let mut csharp_indices = Vec::new();

    for (idx, file_data) in all_files.iter().enumerate() {
        if file_data.lang == "c_sharp" {
            csharp_indices.push(idx);
            continue;
        }

        let local_class_names: HashSet<String> =
            file_data.classes.iter().map(|c| c.name.clone()).collect();

        for class in &file_data.classes {
            for base in &class.bases {
                if let Some(link) = resolve_inheritance_link(
                    &class.name,
                    base,
                    &file_data.path,
                    &local_class_names,
                    &file_data.local_imports,
                    imports_map,
                ) {
                    inheritance_batch.push(link);
                }
            }
        }
    }

    (inheritance_batch, csharp_indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_local_inheritance() {
        let local_classes: HashSet<String> =
            ["Animal"].iter().map(|s| s.to_string()).collect();
        let result = resolve_inheritance_link(
            "Dog",
            "Animal",
            "/file.py",
            &local_classes,
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(result.is_some());
        let link = result.unwrap();
        assert_eq!(link.child_name, "Dog");
        assert_eq!(link.parent_name, "Animal");
        assert_eq!(link.resolved_parent_file_path, "/file.py");
    }

    #[test]
    fn test_resolve_imported_inheritance() {
        let local_classes = HashSet::new();
        let mut imports_map = HashMap::new();
        imports_map.insert("BaseClass".to_string(), vec!["/base.py".to_string()]);

        let result = resolve_inheritance_link(
            "Child",
            "BaseClass",
            "/child.py",
            &local_classes,
            &HashMap::new(),
            &imports_map,
        );
        assert!(result.is_some());
        let link = result.unwrap();
        assert_eq!(link.resolved_parent_file_path, "/base.py");
    }

    #[test]
    fn test_skip_object_base() {
        let result = resolve_inheritance_link(
            "Foo",
            "object",
            "/file.py",
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_build_inheritance() {
        let mut imports_map = HashMap::new();
        imports_map.insert("Base".to_string(), vec!["/base.py".to_string()]);

        let files = vec![FileInheritanceData {
            path: "/child.py".to_string(),
            lang: "python".to_string(),
            classes: vec![ClassInfo {
                name: "Child".to_string(),
                bases: vec!["Base".to_string()],
            }],
            local_imports: HashMap::new(),
        }];

        let (batch, csharp) = build_inheritance_and_csharp_files(&files, &imports_map);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].child_name, "Child");
        assert_eq!(batch[0].parent_name, "Base");
        assert!(csharp.is_empty());
    }
}
