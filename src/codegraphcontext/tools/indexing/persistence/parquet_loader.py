"""Parquet bulk loader for KuzuDB — uses COPY FROM for 10-50x faster DB writes.

Strategy (like GKG/LadybugDB):
  1. Serialize parsed data → Parquet files (type-safe, no escaping issues)
  2. Drop existing tables → Recreate schema
  3. COPY FROM parquet (KuzuDB native bulk import)

This replaces the MERGE-based batch writer for initial indexing.
For incremental updates, the MERGE writer is still available.
"""

from __future__ import annotations

import os
import time
import tempfile
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Tuple
from collections import defaultdict

from ....utils.debug_log import info_logger, warning_logger


def _ensure_pyarrow():
    """Import pyarrow lazily."""
    try:
        import pyarrow as pa
        import pyarrow.parquet as pq
        return pa, pq
    except ImportError:
        raise ImportError(
            "pyarrow is required for Parquet bulk loading. "
            "Install with: pip install pyarrow"
        )


def bulk_load_parquet(
    conn: Any,
    all_file_data: List[Dict[str, Any]],
    repo_path: str,
    call_groups: Tuple[List, List, List, List, List, List],
    inheritance_links: List[Dict] = None,
    on_progress: Optional[Callable] = None,
) -> Dict[str, int]:
    """Bulk load all data via Parquet COPY FROM.

    Args:
        conn: Raw kuzu.Connection (not the wrapper)
        all_file_data: List of parsed file dicts
        repo_path: Repository root path
        call_groups: Tuple of 6 call edge lists (fn_fn, fn_cls, cls_fn, cls_cls, file_fn, file_cls)
        inheritance_links: Optional list of inheritance dicts
        on_progress: Optional progress callback

    Returns:
        Dict with counts of loaded entities.
    """
    pa, pq = _ensure_pyarrow()
    t_start = time.time()
    repo_path_str = str(Path(repo_path).resolve())
    repo_name = Path(repo_path).name

    pq_dir = tempfile.mkdtemp(prefix="cgc_parquet_")
    counts = {}

    def progress(msg):
        if on_progress:
            on_progress(0, 0, msg)
        info_logger(f"[ParquetBulk] {msg}")

    def write_pq(name, table):
        path = os.path.join(pq_dir, f"{name}.parquet")
        pq.write_table(table, path, compression='snappy')
        return path

    # ── Phase 1: Serialize to Parquet ──
    progress("Serializing to Parquet...")
    t0 = time.time()

    # Helper: make uid
    def uid(name, path, line):
        return f"{name}|{path}|{line}"

    # Collect all data
    file_paths, file_names, file_rels, file_deps = [], [], [], []
    dir_paths, dir_names = [], []
    dir_from, dir_to = defaultdict(list), defaultdict(list)  # parent_label → lists
    file_parent_from, file_parent_to = defaultdict(list), defaultdict(list)

    # Symbols per label: {label: {col_name: [values]}}
    sym_data = {}
    sym_contains = {}  # {label: (file_paths, symbol_uids)}

    param_uids, param_names, param_paths, param_flines = [], [], [], []
    param_rel_fn, param_rel_p = [], []

    class_fn_cls, class_fn_fn = [], []
    nested_outer, nested_inner = [], []

    mod_names, mod_fulls = [], []
    imp_file, imp_mod, imp_alias, imp_imported, imp_line = [], [], [], [], []

    seen_dirs = set()
    seen_symbols = set()
    seen_modules = set()

    for fd in all_file_data:
        fp = str(Path(fd["path"]).resolve())
        fname = Path(fp).name
        try:
            rel = str(Path(fp).relative_to(repo_path_str))
        except ValueError:
            rel = fname
        is_dep = fd.get("is_dependency", False)
        lang = fd.get("lang", "")

        file_paths.append(fp)
        file_names.append(fname)
        file_rels.append(rel)
        file_deps.append(is_dep)

        # Directory chain
        try:
            rel_to_file = Path(fp).relative_to(repo_path_str)
        except ValueError:
            rel_to_file = Path(fname)

        parent_path = repo_path_str
        parent_label = "Repository"
        for part in rel_to_file.parts[:-1]:
            cur = str(Path(parent_path) / part)
            if cur not in seen_dirs:
                seen_dirs.add(cur)
                dir_paths.append(cur)
                dir_names.append(part)
                dir_from[parent_label].append(parent_path)
                dir_to[parent_label].append(cur)
            parent_path = cur
            parent_label = "Directory"

        file_parent_from[parent_label].append(parent_path)
        file_parent_to[parent_label].append(fp)

        # Symbols
        label_mappings = [
            ("functions", "Function"), ("classes", "Class"), ("variables", "Variable"),
            ("traits", "Trait"), ("interfaces", "Interface"), ("macros", "Macro"),
            ("structs", "Struct"), ("enums", "Enum"), ("unions", "Union"),
            ("records", "Record"), ("properties", "Property"),
        ]
        for key, label in label_mappings:
            items = fd.get(key, [])
            if not items:
                continue
            if label not in sym_data:
                sym_data[label] = defaultdict(list)
                sym_contains[label] = ([], [])

            for item in items:
                name = item.get("name", "")
                line = item.get("line_number", 0)
                u = uid(name, fp, line)
                if u in seen_symbols:
                    continue
                seen_symbols.add(u)

                d = sym_data[label]
                d["uid"].append(u)
                d["name"].append(name)
                d["path"].append(fp)
                d["line_number"].append(line)
                d["end_line"].append(item.get("end_line", 0))
                d["source"].append(item.get("source", ""))
                d["docstring"].append(item.get("docstring", ""))
                d["lang"].append(item.get("lang", lang))
                d["is_dependency"].append(item.get("is_dependency", is_dep))

                # Function-specific fields
                if label == "Function":
                    d["cyclomatic_complexity"].append(item.get("cyclomatic_complexity", 1))
                    d["context"].append(item.get("context", ""))
                    d["context_type"].append(item.get("context_type", ""))
                    d["class_context"].append(item.get("class_context", ""))

                    args = item.get("args", [])
                    decorators = item.get("decorators", [])
                    d["args"].append(args if args else [])
                    d["decorators"].append(decorators if decorators else [])

                    # Parameters
                    for arg_name in args:
                        if not arg_name:
                            continue
                        pu = f"{arg_name}|{fp}|{line}"
                        param_uids.append(pu)
                        param_names.append(arg_name)
                        param_paths.append(fp)
                        param_flines.append(line)
                        param_rel_fn.append(u)
                        param_rel_p.append(pu)

                    # Class containment
                    cc = item.get("class_context", "")
                    if cc:
                        class_fn_cls.append(f"{cc}|{fp}")
                        class_fn_fn.append(u)

                    # Nested functions
                    if item.get("context_type") == "function_definition" and item.get("context"):
                        nested_outer.append(f"{item['context']}|{fp}")
                        nested_inner.append(u)

                elif label == "Class":
                    d.setdefault("decorators", []).append(item.get("decorators", []) or [])

                elif label == "Variable":
                    d["value"] = d.get("value", [])
                    d["value"].append(item.get("value", ""))
                    d["context"] = d.get("context", [])
                    d["context"].append(item.get("context", ""))

                sym_contains[label][0].append(fp)
                sym_contains[label][1].append(u)

        # Imports
        for imp in fd.get("imports", []):
            name = imp.get("name", "")
            alias = imp.get("alias", "")
            source = imp.get("source", "")
            # Bug 1 fix: full_import_name fallback chain
            full = imp.get("full_import_name", "") or source or name
            line_n = imp.get("line_number", 0)
            mod_name = source if source else name
            imported = name if source else ""
            if not mod_name:
                continue

            if mod_name not in seen_modules:
                seen_modules.add(mod_name)
                mod_names.append(mod_name)
                mod_fulls.append(full)

            imp_file.append(fp)
            imp_mod.append(mod_name)
            imp_alias.append(alias)
            imp_imported.append(imported)
            imp_line.append(line_n)

    t_serialize_data = time.time() - t0

    # ── Write Parquet files ──
    t0 = time.time()
    pq_files = {}

    # Repository (single row, COPY FROM)
    pq_files["Repository"] = write_pq("repository", pa.table({
        "path": [repo_path_str],
        "name": [repo_name],
        "is_dependency": [False],
    }))

    # Files
    if file_paths:
        pq_files["File"] = write_pq("files", pa.table({
            "path": file_paths,
            "name": file_names,
            "relative_path": file_rels,
            "is_dependency": file_deps,
        }))
        counts["files"] = len(file_paths)

    # Directories
    if dir_paths:
        pq_files["Directory"] = write_pq("directories", pa.table({
            "path": dir_paths,
            "name": dir_names,
        }))
        counts["directories"] = len(dir_paths)

    # Symbols
    for label, d in sym_data.items():
        if label == "Function":
            tbl = pa.table({
                "uid": d["uid"], "name": d["name"], "path": d["path"],
                "line_number": pa.array(d["line_number"], type=pa.int64()),
                "end_line": pa.array(d["end_line"], type=pa.int64()),
                "source": d["source"], "docstring": d["docstring"],
                "lang": d["lang"],
                "cyclomatic_complexity": pa.array(d["cyclomatic_complexity"], type=pa.int64()),
                "context": d["context"], "context_type": d["context_type"],
                "class_context": d["class_context"],
                "is_dependency": d["is_dependency"],
                "decorators": pa.array(d["decorators"], type=pa.list_(pa.string())),
                "args": pa.array(d["args"], type=pa.list_(pa.string())),
            })
        elif label == "Class":
            tbl = pa.table({
                "uid": d["uid"], "name": d["name"], "path": d["path"],
                "line_number": pa.array(d["line_number"], type=pa.int64()),
                "end_line": pa.array(d["end_line"], type=pa.int64()),
                "source": d["source"], "docstring": d["docstring"],
                "lang": d["lang"], "is_dependency": d["is_dependency"],
                "decorators": pa.array(d.get("decorators", [[] for _ in d["uid"]]), type=pa.list_(pa.string())),
            })
        elif label == "Variable":
            tbl = pa.table({
                "uid": d["uid"], "name": d["name"], "path": d["path"],
                "line_number": pa.array(d["line_number"], type=pa.int64()),
                "source": d["source"], "docstring": d["docstring"],
                "lang": d["lang"],
                "value": d.get("value", ["" for _ in d["uid"]]),
                "context": d.get("context", ["" for _ in d["uid"]]),
                "is_dependency": d["is_dependency"],
            })
        elif label == "Parameter":
            continue  # handled separately
        else:
            # Generic: Trait, Interface, Macro, Struct, Enum, Union, Annotation, Record, Property
            tbl = pa.table({
                "uid": d["uid"], "name": d["name"], "path": d["path"],
                "line_number": pa.array(d["line_number"], type=pa.int64()),
                "end_line": pa.array(d["end_line"], type=pa.int64()),
                "source": d["source"], "docstring": d["docstring"],
                "lang": d["lang"], "is_dependency": d["is_dependency"],
            })

        pq_files[label] = write_pq(f"sym_{label.lower()}", tbl)
        counts[f"sym_{label.lower()}"] = len(d["uid"])

    # Parameters
    if param_uids:
        pq_files["Parameter"] = write_pq("params", pa.table({
            "uid": param_uids,
            "name": param_names,
            "path": param_paths,
            "function_line_number": pa.array(param_flines, type=pa.int64()),
        }))
        counts["params"] = len(param_uids)

    # Modules
    if mod_names:
        pq_files["Module"] = write_pq("modules", pa.table({
            "name": mod_names,
            "lang": ["" for _ in mod_names],
            "full_import_name": mod_fulls,
        }))
        counts["modules"] = len(mod_names)

    # ── Relationship Parquet files ──
    rel_files = {}

    # Directory CONTAINS (Repository→Directory, Directory→Directory)
    for parent_label in ("Repository", "Directory"):
        froms = dir_from.get(parent_label, [])
        tos = dir_to.get(parent_label, [])
        if froms:
            key = f"CONTAINS_{parent_label}_Directory"
            rel_files[key] = {
                "path": write_pq(f"rel_contains_{parent_label.lower()}_dir", pa.table({
                    "from": froms, "to": tos,
                })),
                "from": parent_label, "to": "Directory",
            }

    # File parent CONTAINS (Repository→File, Directory→File)
    for parent_label in ("Repository", "Directory"):
        froms = file_parent_from.get(parent_label, [])
        tos = file_parent_to.get(parent_label, [])
        if froms:
            key = f"CONTAINS_{parent_label}_File"
            rel_files[key] = {
                "path": write_pq(f"rel_contains_{parent_label.lower()}_file", pa.table({
                    "from": froms, "to": tos,
                })),
                "from": parent_label, "to": "File",
            }

    # File CONTAINS Symbol
    for label, (fps, uids) in sym_contains.items():
        if fps:
            escaped = label
            key = f"CONTAINS_File_{label}"
            rel_files[key] = {
                "path": write_pq(f"rel_file_contains_{label.lower()}", pa.table({
                    "from": fps, "to": uids,
                })),
                "from": "File", "to": escaped,
            }

    # HAS_PARAMETER
    if param_rel_fn:
        rel_files["HAS_PARAMETER"] = {
            "path": write_pq("rel_has_parameter", pa.table({
                "from": param_rel_fn, "to": param_rel_p,
            })),
            "from": "Function", "to": "Parameter",
        }

    # IMPORTS (File→Module)
    if imp_file:
        rel_files["IMPORTS"] = {
            "path": write_pq("rel_imports", pa.table({
                "from": imp_file, "to": imp_mod,
                "alias": imp_alias,
                "full_import_name": imp_imported,
                "imported_name": imp_imported,
                "line_number": pa.array(imp_line, type=pa.int64()),
            })),
            "from": "File", "to": "Module",
        }

    # CALLS (6 categories)
    # Build (name, path) → uid lookup for resolving called targets
    func_uid_lookup = {}  # (name, path) → first uid
    class_uid_lookup = {}
    for label, d in sym_data.items():
        lookup = func_uid_lookup if label == "Function" else class_uid_lookup if label == "Class" else None
        if lookup is None:
            continue
        for i, u in enumerate(d["uid"]):
            key = (d["name"][i], d["path"][i])
            if key not in lookup:
                lookup[key] = u

    call_labels = [
        ("fn_fn", "Function", "Function", func_uid_lookup),
        ("fn_cls", "Function", "Class", class_uid_lookup),
        ("cls_fn", "Class", "Function", func_uid_lookup),
        ("cls_cls", "Class", "Class", class_uid_lookup),
        ("file_fn", "File", "Function", func_uid_lookup),
        ("file_cls", "File", "Class", class_uid_lookup),
    ]
    fn_to_fn, fn_to_cls, cls_to_fn, cls_to_cls, file_to_fn, file_to_cls = call_groups
    call_lists = [fn_to_fn, fn_to_cls, cls_to_fn, cls_to_cls, file_to_fn, file_to_cls]

    for (name, from_label, to_label, to_lookup), edges in zip(call_labels, call_lists):
        if not edges:
            continue
        froms, tos, lines, args_list, fcns = [], [], [], [], []
        skipped = 0
        for e in edges:
            # Resolve called target uid
            called_key = (e.get("called_name", ""), e.get("called_file_path", ""))
            called_uid = to_lookup.get(called_key)
            if not called_uid:
                skipped += 1
                continue

            # Resolve caller
            if from_label == "File":
                froms.append(e.get("caller_file_path", ""))
            else:
                caller_label_lookup = func_uid_lookup if from_label == "Function" else class_uid_lookup
                caller_uid = uid(
                    e.get("caller_name", ""),
                    e.get("caller_file_path", ""),
                    e.get("caller_line_number", 0),
                )
                froms.append(caller_uid)

            tos.append(called_uid)
            lines.append(e.get("line_number", 0))
            args_list.append(e.get("args", []) or [])
            fcns.append(e.get("full_call_name", ""))

        if froms:
            key = f"CALLS_{name}"
            rel_files[key] = {
                "path": write_pq(f"rel_calls_{name}", pa.table({
                    "from": froms,
                    "to": tos,
                    "line_number": pa.array(lines, type=pa.int64()),
                    "args": pa.array(args_list, type=pa.list_(pa.string())),
                    "full_call_name": fcns,
                })),
                "from": from_label, "to": to_label,
                "is_calls": True,
            }
        counts[f"calls_{name}"] = len(froms)
        if skipped:
            info_logger(f"[ParquetBulk] CALLS {name}: {len(froms)} written, {skipped} skipped (target not found)")

    # INHERITS
    if inheritance_links:
        child_uids, parent_uids = [], []
        for link in inheritance_links:
            child_uids.append(uid(
                link.get("child_name", ""),
                link.get("child_path", ""),
                link.get("child_line", 0),
            ))
            parent_uids.append(uid(
                link.get("parent_name", ""),
                link.get("parent_path", ""),
                link.get("parent_line", 0),
            ))
        rel_files["INHERITS_Class_Class"] = {
            "path": write_pq("rel_inherits", pa.table({
                "from": child_uids, "to": parent_uids,
            })),
            "from": "Class", "to": "Class",
        }
        counts["inheritance"] = len(inheritance_links)

    t_write_pq = time.time() - t0
    progress(f"Parquet written in {t_write_pq:.1f}s ({len(pq_files)} node files, {len(rel_files)} rel files)")

    # ── Phase 2: Drop & recreate tables, COPY FROM ──
    progress("Dropping and recreating tables...")
    t0 = time.time()

    _drop_all_tables(conn)
    _create_schema(conn)

    t_schema = time.time() - t0

    # ── Phase 3: COPY FROM ──
    progress("COPY FROM parquet (bulk import)...")
    t0 = time.time()

    # Node tables first
    node_order = [
        "Repository", "File", "Directory", "Module", "Parameter",
        "Function", "Class", "Variable", "Trait", "Interface",
        "Macro", "Struct", "Enum", "Union", "Annotation", "Record", "Property",
    ]
    for table_name in node_order:
        if table_name in pq_files:
            escaped = f"`{table_name}`" if table_name in ("Macro", "Union", "Property") else table_name
            path = pq_files[table_name]
            try:
                conn.execute(f"COPY {escaped} FROM '{path}'")
            except Exception as e:
                warning_logger(f"COPY {table_name} failed: {e}")

    # Relationship tables
    for key, info in rel_files.items():
        path = info["path"]
        from_t = info["from"]
        to_t = info["to"]
        is_calls = info.get("is_calls", False)

        # Determine rel table name
        if "CONTAINS" in key:
            rel_name = "CONTAINS"
        elif "HAS_PARAMETER" in key:
            rel_name = "HAS_PARAMETER"
        elif "IMPORTS" in key:
            rel_name = "IMPORTS"
        elif "CALLS" in key:
            rel_name = "CALLS"
        elif "INHERITS" in key:
            rel_name = "INHERITS"
        else:
            continue

        # Escape reserved words in from/to
        from_escaped = f"`{from_t}`" if from_t in ("Macro", "Union", "Property") else from_t
        to_escaped = f"`{to_t}`" if to_t in ("Macro", "Union", "Property") else to_t

        try:
            conn.execute(
                f"COPY {rel_name} FROM '{path}' (from='{from_escaped}', to='{to_escaped}')"
            )
        except Exception as e:
            warning_logger(f"COPY {rel_name} ({from_t}→{to_t}) failed: {e}")

    t_copy = time.time() - t0

    # Cleanup temp dir
    import shutil
    shutil.rmtree(pq_dir, ignore_errors=True)

    total = time.time() - t_start
    progress(f"Done in {total:.1f}s (serialize={t_serialize_data:.1f}s, write_pq={t_write_pq:.1f}s, schema={t_schema:.1f}s, copy={t_copy:.1f}s)")
    counts["total_time"] = round(total, 1)
    return counts


def _drop_all_tables(conn):
    """Drop all node and rel tables."""
    # Drop rel tables first (depend on node tables)
    for rel in ["CALLS", "CONTAINS", "IMPORTS", "INHERITS", "HAS_PARAMETER", "INCLUDES", "IMPLEMENTS"]:
        try:
            conn.execute(f"DROP TABLE {rel}")
        except Exception:
            pass

    # Drop node tables
    for node in ["Parameter", "Function", "Class", "Variable", "Trait", "Interface",
                  "`Macro`", "Struct", "Enum", "`Union`", "Annotation", "Record",
                  "`Property`", "Module", "File", "Directory", "Repository"]:
        try:
            conn.execute(f"DROP TABLE {node}")
        except Exception:
            pass


def _create_schema(conn):
    """Recreate all node and rel tables."""
    node_tables = [
        ("Repository", "path STRING, name STRING, is_dependency BOOLEAN, PRIMARY KEY (path)"),
        ("File", "path STRING, name STRING, relative_path STRING, is_dependency BOOLEAN, PRIMARY KEY (path)"),
        ("Directory", "path STRING, name STRING, PRIMARY KEY (path)"),
        ("Module", "name STRING, lang STRING, full_import_name STRING, PRIMARY KEY (name)"),
        ("Function", "uid STRING, name STRING, path STRING, line_number INT64, end_line INT64, source STRING, docstring STRING, lang STRING, cyclomatic_complexity INT64, context STRING, context_type STRING, class_context STRING, is_dependency BOOLEAN, decorators STRING[], args STRING[], PRIMARY KEY (uid)"),
        ("Class", "uid STRING, name STRING, path STRING, line_number INT64, end_line INT64, source STRING, docstring STRING, lang STRING, is_dependency BOOLEAN, decorators STRING[], PRIMARY KEY (uid)"),
        ("Variable", "uid STRING, name STRING, path STRING, line_number INT64, source STRING, docstring STRING, lang STRING, value STRING, context STRING, is_dependency BOOLEAN, PRIMARY KEY (uid)"),
        ("Trait", "uid STRING, name STRING, path STRING, line_number INT64, end_line INT64, source STRING, docstring STRING, lang STRING, is_dependency BOOLEAN, PRIMARY KEY (uid)"),
        ("Interface", "uid STRING, name STRING, path STRING, line_number INT64, end_line INT64, source STRING, docstring STRING, lang STRING, is_dependency BOOLEAN, PRIMARY KEY (uid)"),
        ("Macro", "uid STRING, name STRING, path STRING, line_number INT64, end_line INT64, source STRING, docstring STRING, lang STRING, is_dependency BOOLEAN, PRIMARY KEY (uid)"),
        ("Struct", "uid STRING, name STRING, path STRING, line_number INT64, end_line INT64, source STRING, docstring STRING, lang STRING, is_dependency BOOLEAN, PRIMARY KEY (uid)"),
        ("Enum", "uid STRING, name STRING, path STRING, line_number INT64, end_line INT64, source STRING, docstring STRING, lang STRING, is_dependency BOOLEAN, PRIMARY KEY (uid)"),
        ("Union", "uid STRING, name STRING, path STRING, line_number INT64, end_line INT64, source STRING, docstring STRING, lang STRING, is_dependency BOOLEAN, PRIMARY KEY (uid)"),
        ("Annotation", "uid STRING, name STRING, path STRING, line_number INT64, end_line INT64, source STRING, docstring STRING, lang STRING, is_dependency BOOLEAN, PRIMARY KEY (uid)"),
        ("Record", "uid STRING, name STRING, path STRING, line_number INT64, end_line INT64, source STRING, docstring STRING, lang STRING, is_dependency BOOLEAN, PRIMARY KEY (uid)"),
        ("Property", "uid STRING, name STRING, path STRING, line_number INT64, end_line INT64, source STRING, docstring STRING, lang STRING, is_dependency BOOLEAN, PRIMARY KEY (uid)"),
        ("Parameter", "uid STRING, name STRING, path STRING, function_line_number INT64, PRIMARY KEY (uid)"),
    ]
    rel_tables = [
        ("CONTAINS", "FROM File TO Function, FROM File TO Class, FROM File TO Variable, FROM File TO Trait, FROM File TO Interface, FROM `Macro` TO `Macro`, FROM File TO `Macro`, FROM File TO Struct, FROM File TO Enum, FROM File TO `Union`, FROM File TO Annotation, FROM File TO Record, FROM File TO `Property`, FROM Repository TO Directory, FROM Directory TO Directory, FROM Directory TO File, FROM Repository TO File, FROM Class TO Function, FROM Function TO Function", True),
        ("CALLS", "FROM Function TO Function, FROM Function TO Class, FROM File TO Function, FROM File TO Class, FROM Class TO Function, FROM Class TO Class, line_number INT64, args STRING[], full_call_name STRING", True),
        ("IMPORTS", "FROM File TO Module, alias STRING, full_import_name STRING, imported_name STRING, line_number INT64", False),
        ("INHERITS", "FROM Class TO Class, FROM Record TO Record, FROM Interface TO Interface", True),
        ("HAS_PARAMETER", "FROM Function TO Parameter", False),
        ("INCLUDES", "FROM Class TO Module", False),
        ("IMPLEMENTS", "FROM Class TO Interface, FROM Struct TO Interface, FROM Record TO Interface", True),
    ]

    for table_name, schema in node_tables:
        try:
            conn.execute(f"CREATE NODE TABLE `{table_name}`({schema})")
        except Exception:
            pass

    for table_name, schema, use_group in rel_tables:
        try:
            if use_group:
                conn.execute(f"CREATE REL TABLE GROUP `{table_name}`({schema})")
            else:
                conn.execute(f"CREATE REL TABLE `{table_name}`({schema})")
        except Exception:
            pass
