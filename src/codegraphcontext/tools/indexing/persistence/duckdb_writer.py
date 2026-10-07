"""DuckDB graph writer using Parquet COPY FROM for ~45x faster DB writes.

Architecture:
  Rust parse → Python collect → PyArrow Parquet → DuckDB CREATE AS SELECT

Usage:
    from codegraphcontext.tools.indexing.persistence.duckdb_writer import DuckDBGraphWriter

    writer = DuckDBGraphWriter(db_path)
    writer.write_all(parsed_results, repo_path, call_groups, inheritance)
    # Query:
    top = writer.get_top_connected(limit=20)
    edges = writer.get_call_graph(file_paths)
    writer.close()
"""

from __future__ import annotations

import logging
import os
import tempfile
import time
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Tuple

logger = logging.getLogger(__name__)


def _safe_str(v: Any) -> str:
    if v is None:
        return ""
    if isinstance(v, list):
        import json
        return json.dumps(v)
    return str(v)


class DuckDBGraphWriter:
    """High-performance graph writer backed by DuckDB + Parquet bulk load."""

    def __init__(self, db_path: str):
        self._conn = None  # set before connect to prevent __del__ crash
        self._schema_created = False

        try:
            import duckdb
        except ImportError:
            raise ImportError("duckdb not installed. Run: pip install duckdb")

        self.db_path = db_path
        os.makedirs(os.path.dirname(db_path) or ".", exist_ok=True)
        self._conn = duckdb.connect(db_path)

    def close(self):
        if hasattr(self, "_conn") and self._conn:
            self._conn.close()
            self._conn = None

    def __del__(self):
        self.close()

    # ── Schema ───────────────────────────────────────────────────────

    def _create_schema(self):
        if self._schema_created:
            return
        c = self._conn
        c.execute("""CREATE TABLE IF NOT EXISTS repository (
            path VARCHAR PRIMARY KEY, name VARCHAR)""")
        c.execute("""CREATE TABLE IF NOT EXISTS files (
            path VARCHAR PRIMARY KEY, name VARCHAR,
            relative_path VARCHAR, is_dependency BOOLEAN DEFAULT FALSE)""")
        c.execute("""CREATE TABLE IF NOT EXISTS directories (
            path VARCHAR PRIMARY KEY, name VARCHAR, parent_path VARCHAR)""")
        c.execute("""CREATE TABLE IF NOT EXISTS functions (
            uid VARCHAR PRIMARY KEY, name VARCHAR, path VARCHAR,
            line_number INTEGER, complexity INTEGER DEFAULT 0,
            return_type VARCHAR DEFAULT '', docstring VARCHAR DEFAULT '',
            class_context VARCHAR DEFAULT '', is_async BOOLEAN DEFAULT FALSE,
            body_start_line INTEGER DEFAULT 0, body_end_line INTEGER DEFAULT 0,
            decorators VARCHAR DEFAULT '')""")
        c.execute("""CREATE TABLE IF NOT EXISTS classes (
            uid VARCHAR PRIMARY KEY, name VARCHAR, path VARCHAR,
            line_number INTEGER, docstring VARCHAR DEFAULT '',
            bases VARCHAR DEFAULT '', decorators VARCHAR DEFAULT '',
            kind VARCHAR DEFAULT 'class')""")
        # Dependency injection: injector class -> injected bean type
        # (Spring @Autowired/@Inject fields, Lombok final fields, constructors)
        c.execute("""CREATE TABLE IF NOT EXISTS injections (
            injector_class VARCHAR, injector_path VARCHAR,
            injected_type VARCHAR, injected_path VARCHAR DEFAULT '',
            field_name VARCHAR DEFAULT '', line_number INTEGER DEFAULT 0,
            kind VARCHAR DEFAULT '', stereotype VARCHAR DEFAULT '')""")
        c.execute("""CREATE TABLE IF NOT EXISTS variables (
            uid VARCHAR PRIMARY KEY, name VARCHAR, path VARCHAR,
            line_number INTEGER, type VARCHAR DEFAULT '')""")
        c.execute("""CREATE TABLE IF NOT EXISTS parameters (
            uid VARCHAR PRIMARY KEY, name VARCHAR, path VARCHAR,
            function_uid VARCHAR, function_line_number INTEGER)""")
        c.execute("""CREATE TABLE IF NOT EXISTS modules (
            name VARCHAR PRIMARY KEY)""")
        c.execute("""CREATE TABLE IF NOT EXISTS imports (
            file_path VARCHAR, module_name VARCHAR,
            imported_name VARCHAR DEFAULT '', alias VARCHAR DEFAULT '',
            full_import_name VARCHAR DEFAULT '', line_number INTEGER DEFAULT 0)""")
        c.execute("""CREATE TABLE IF NOT EXISTS calls (
            caller_uid VARCHAR, called_uid VARCHAR,
            caller_type VARCHAR, called_type VARCHAR,
            caller_name VARCHAR DEFAULT '', called_name VARCHAR DEFAULT '',
            caller_path VARCHAR DEFAULT '', called_path VARCHAR DEFAULT '',
            line_number INTEGER DEFAULT 0, full_call_name VARCHAR DEFAULT '',
            confidence VARCHAR DEFAULT 'EXTRACTED',
            resolution_tier INTEGER DEFAULT 0,
            receiver_type VARCHAR DEFAULT '')""")
        # ORM: entity -> table, repository -> entity, code -> table access
        c.execute("""CREATE TABLE IF NOT EXISTS orm_entities (
            entity_class VARCHAR, entity_path VARCHAR, table_name VARCHAR,
            schema VARCHAR DEFAULT '', datastore VARCHAR DEFAULT '',
            line_number INTEGER DEFAULT 0)""")
        c.execute("""CREATE TABLE IF NOT EXISTS orm_repositories (
            repository VARCHAR, repository_path VARCHAR, entity_class VARCHAR,
            entity_path VARCHAR DEFAULT '', table_name VARCHAR DEFAULT '',
            schema VARCHAR DEFAULT '')""")
        c.execute("""CREATE TABLE IF NOT EXISTS db_access (
            caller_class VARCHAR DEFAULT '', caller_name VARCHAR DEFAULT '',
            caller_path VARCHAR, line_number INTEGER DEFAULT 0,
            table_name VARCHAR, schema VARCHAR DEFAULT '',
            operation VARCHAR, via VARCHAR, repository VARCHAR DEFAULT '',
            method VARCHAR DEFAULT '')""")
        c.execute("""CREATE TABLE IF NOT EXISTS inheritance (
            child_uid VARCHAR, parent_uid VARCHAR,
            child_name VARCHAR DEFAULT '', parent_name VARCHAR DEFAULT '',
            child_path VARCHAR DEFAULT '', parent_path VARCHAR DEFAULT '')""")
        c.execute("""CREATE TABLE IF NOT EXISTS file_contains (
            file_path VARCHAR, symbol_uid VARCHAR, symbol_type VARCHAR)""")
        c.execute("""CREATE TABLE IF NOT EXISTS execution_flows (
            name VARCHAR, entry_file VARCHAR, entry_line INTEGER DEFAULT 0,
            entry_class VARCHAR DEFAULT '', step_count INTEGER DEFAULT 0,
            depth INTEGER DEFAULT 0, score INTEGER DEFAULT 0,
            steps_json VARCHAR DEFAULT '[]')""")
        c.execute("""CREATE TABLE IF NOT EXISTS rationales (
            tag VARCHAR, text VARCHAR, file VARCHAR,
            line INTEGER DEFAULT 0, context VARCHAR DEFAULT '')""")
        c.execute("""CREATE TABLE IF NOT EXISTS routes (
            method VARCHAR, path VARCHAR, handler VARCHAR,
            file VARCHAR, line INTEGER DEFAULT 0,
            framework VARCHAR DEFAULT '')""")
        c.execute("""CREATE TABLE IF NOT EXISTS operational_params (
            name VARCHAR, value VARCHAR, path VARCHAR,
            line_number INTEGER DEFAULT 0, category VARCHAR DEFAULT '')""")
        self._schema_created = True

    # ── Main write method ────────────────────────────────────────────

    def write_all(
        self,
        parsed_results: List[Dict[str, Any]],
        repo_path: str,
        call_groups: Tuple,
        inheritance: List[Dict] = None,
        on_progress: Optional[Callable] = None,
    ) -> Dict[str, int]:
        """Write entire graph via Parquet bulk load.

        Returns dict of counts: {files, functions, classes, variables, calls, ...}
        """
        import pyarrow as pa
        import pyarrow.parquet as pq

        repo_path_obj = Path(repo_path).resolve()
        total = len(parsed_results)
        t_start = time.perf_counter()

        if on_progress:
            on_progress(0, total, "Collecting graph data...")

        # ── Collect into columnar lists ──────────────────────────────
        # Files
        f_path = []; f_name = []; f_rel = []; f_dep = []
        # Directories
        dir_set: Dict[str, Tuple[str, str]] = {}  # path → (name, parent)
        # Functions
        fn_uid = []; fn_name = []; fn_path = []; fn_line = []
        fn_cx = []; fn_rt = []; fn_doc = []; fn_cc = []; fn_async = []
        fn_bstart = []; fn_bend = []; fn_dec = []
        # Classes
        cl_uid = []; cl_name = []; cl_path = []; cl_line = []; cl_doc = []; cl_bases = []; cl_dec = []; cl_kind = []
        # Injections (target path resolved after all classes are known)
        inj_rows = []  # (injector_class, injector_rel, injected_type, field, line, kind, stereotype, imports)
        orm_rows = []  # (rel path, orm mapping dict)
        # Variables
        v_uid = []; v_name = []; v_path = []; v_line = []; v_type = []
        # Parameters
        p_uid = []; p_name = []; p_path = []; p_func_uid = []; p_func_line = []
        # Imports
        im_fp = []; im_mod = []; im_name = []; im_alias = []; im_full = []; im_line = []
        # File-contains
        fc_fp = []; fc_uid = []; fc_type = []
        # Modules
        mod_set = set()

        func_set = set(); class_set = set(); var_set = set(); param_set = set()

        for r in parsed_results:
            fp = str(Path(r["path"]).resolve())
            fname = Path(fp).name
            try:
                rel = str(Path(fp).relative_to(repo_path_obj))
            except ValueError:
                rel = fname
            is_dep = r.get("is_dependency", False)
            f_path.append(fp); f_name.append(fname); f_rel.append(rel); f_dep.append(is_dep)

            # Directories
            try:
                rel_parts = Path(fp).relative_to(repo_path_obj).parts[:-1]
            except ValueError:
                rel_parts = ()
            parent = str(repo_path_obj)
            for part in rel_parts:
                dp = str(Path(parent) / part)
                if dp not in dir_set:
                    dir_set[dp] = (part, parent)
                parent = dp

            lang = r.get("lang", "")

            # Functions — store relative path for consistency with routes/rationales
            for fn in r.get("functions", []):
                uid = f"{fn.get('name', '')}|{fp}|{fn.get('line_number', 0)}"
                if uid not in func_set:
                    func_set.add(uid)
                    fn_uid.append(uid); fn_name.append(fn.get("name", "")); fn_path.append(rel)
                    fn_line.append(fn.get("line_number", 0))
                    fn_cx.append(fn.get("complexity", 0) or 0)
                    fn_rt.append(fn.get("return_type", "") or "")
                    fn_doc.append(fn.get("docstring", "") or "")
                    fn_cc.append(fn.get("class_context", "") or "")
                    fn_async.append(fn.get("is_async", False) or False)
                    fn_bstart.append(fn.get("body_start_line", 0) or 0)
                    fn_bend.append(fn.get("body_end_line", 0) or 0)
                    fn_dec.append("\n".join(fn.get("decorators", []) or []))
                    fc_fp.append(rel); fc_uid.append(uid); fc_type.append("Function")

                    # Parameters
                    for arg in fn.get("args", []) or []:
                        arg_name = arg if isinstance(arg, str) else str(arg)
                        if arg_name:
                            puid = f"{arg_name}|{fp}|{fn.get('line_number', 0)}"
                            if puid not in param_set:
                                param_set.add(puid)
                                p_uid.append(puid); p_name.append(arg_name)
                                p_path.append(rel); p_func_uid.append(uid)
                                p_func_line.append(fn.get("line_number", 0))

            # Classes
            for cls in r.get("classes", []):
                uid = f"{cls.get('name', '')}|{fp}|{cls.get('line_number', 0)}"
                if uid not in class_set:
                    class_set.add(uid)
                    cl_uid.append(uid); cl_name.append(cls.get("name", ""))
                    cl_path.append(rel); cl_line.append(cls.get("line_number", 0))
                    cl_doc.append(cls.get("docstring", "") or "")
                    bases = cls.get("bases", [])
                    cl_bases.append(",".join(str(b) for b in bases) if bases else "")
                    cl_dec.append("\n".join(cls.get("decorators", []) or []))
                    cl_kind.append(cls.get("kind") or "class")
                    fc_fp.append(rel); fc_uid.append(uid); fc_type.append("Class")

            for m in r.get("orm_mappings") or []:
                orm_rows.append((rel, m))

            # Injections
            if r.get("injections"):
                file_imports = [i.get("name", "") for i in r.get("imports", []) or []]
                for inj in r["injections"]:
                    inj_rows.append((
                        inj.get("injector_class", ""), rel, inj.get("injected_type", ""),
                        inj.get("field_name", ""), inj.get("line_number", 0),
                        inj.get("kind", ""), inj.get("stereotype") or "", file_imports,
                    ))

            # Variables
            for var in r.get("variables", []):
                uid = f"{var.get('name', '')}|{fp}|{var.get('line_number', 0)}"
                if uid not in var_set:
                    var_set.add(uid)
                    v_uid.append(uid); v_name.append(var.get("name", ""))
                    v_path.append(rel); v_line.append(var.get("line_number", 0))
                    v_type.append(var.get("type", "") or "")
                    fc_fp.append(rel); fc_uid.append(uid); fc_type.append("Variable")

            # Imports
            for imp in r.get("imports", []):
                source = imp.get("source", "") or imp.get("name", "")
                name = imp.get("name", "")
                alias = imp.get("alias", "") or ""
                full = imp.get("full_import_name", "") or source or name
                ln = imp.get("line_number", 0)
                mod_name = source or name
                if mod_name:
                    mod_set.add(mod_name)
                im_fp.append(fp); im_mod.append(mod_name); im_name.append(name)
                im_alias.append(alias); im_full.append(full); im_line.append(ln)

        if on_progress:
            on_progress(total // 3, total, "Writing Parquet files...")

        # ── CALLS edges ──────────────────────────────────────────────
        c_caller = []; c_called = []; c_ct = []; c_cdt = []
        c_cn = []; c_dn = []; c_cp = []; c_dp = []
        c_ln = []; c_fcn = []; c_conf = []; c_tier = []; c_recv = []
        edge_labels = [
            ("Function", "Function"), ("Function", "Class"),
            ("Class", "Function"), ("Class", "Class"),
            ("File", "Function"), ("File", "Class"),
        ]
        for (ct, cdt), group in zip(edge_labels, call_groups):
            for e in group:
                if ct == "File":
                    cuid = e.get("caller_file_path", "")
                else:
                    cuid = f"{e.get('caller_name', '')}|{e.get('caller_file_path', '')}|{e.get('caller_line_number', 0)}"
                duid = f"{e.get('called_name', '')}|{e.get('called_file_path', '')}|0"
                c_caller.append(cuid); c_called.append(duid); c_ct.append(ct); c_cdt.append(cdt)
                c_cn.append(e.get("caller_name", "")); c_dn.append(e.get("called_name", ""))
                caller_path = e.get("caller_file_path", "")
                called_path = e.get("called_file_path", "")
                # Normalize to relative paths
                try:
                    caller_path = str(Path(caller_path).relative_to(repo_path_obj)) if caller_path else ""
                except ValueError:
                    pass
                try:
                    called_path = str(Path(called_path).relative_to(repo_path_obj)) if called_path else ""
                except ValueError:
                    pass
                c_cp.append(caller_path); c_dp.append(called_path)
                c_ln.append(e.get("line_number", 0)); c_fcn.append(e.get("full_call_name", ""))
                # Confidence comes from the resolver's tier (upstream scheme):
                # EXTRACTED / INFERRED / AMBIGUOUS. Older resolvers without a
                # tier fall back to the same-file heuristic.
                tier = e.get("resolution_tier", 0)
                c_tier.append(tier)
                c_recv.append(e.get("receiver_type") or "")
                c_conf.append(e.get("confidence") or (
                    "EXTRACTED" if caller_path == called_path else "INFERRED"))

        # ── Inheritance ──────────────────────────────────────────────
        inh_child = []; inh_parent = []; inh_cn = []; inh_pn = []; inh_cp = []; inh_pp = []
        for edge in (inheritance or []):
            child_name = edge.get("child_name", "")
            parent_name = edge.get("parent_name", "")
            # Rust resolver emits `path` / `resolved_parent_file_path` (same
            # contract as the graph writer); keep the old keys as fallback.
            child_path = edge.get("path") or edge.get("child_file_path", "")
            parent_path = edge.get("resolved_parent_file_path") or edge.get("parent_file_path", "")
            # Normalize to relative paths
            try:
                child_path = str(Path(child_path).relative_to(repo_path_obj)) if child_path else ""
            except ValueError:
                pass
            try:
                parent_path = str(Path(parent_path).relative_to(repo_path_obj)) if parent_path else ""
            except ValueError:
                pass
            inh_child.append(f"{child_name}|{child_path}|0")
            inh_parent.append(f"{parent_name}|{parent_path}|0")
            inh_cn.append(child_name); inh_pn.append(parent_name)
            inh_cp.append(child_path); inh_pp.append(parent_path)

        # ── Write Parquet ────────────────────────────────────────────
        _t_pq = time.perf_counter()
        pq_dir = tempfile.mkdtemp(prefix="cgc_pq_")

        pq.write_table(pa.table({
            "path": f_path, "name": f_name, "relative_path": f_rel, "is_dependency": f_dep,
        }), f"{pq_dir}/files.parquet")

        if dir_set:
            d_paths = list(dir_set.keys())
            d_names = [dir_set[p][0] for p in d_paths]
            d_parents = [dir_set[p][1] for p in d_paths]
            pq.write_table(pa.table({
                "path": d_paths, "name": d_names, "parent_path": d_parents,
            }), f"{pq_dir}/directories.parquet")

        pq.write_table(pa.table({
            "uid": fn_uid, "name": fn_name, "path": fn_path, "line_number": fn_line,
            "complexity": fn_cx, "return_type": fn_rt, "docstring": fn_doc,
            "class_context": fn_cc, "is_async": fn_async,
            "body_start_line": fn_bstart, "body_end_line": fn_bend,
            "decorators": fn_dec,
        }), f"{pq_dir}/functions.parquet")

        pq.write_table(pa.table({
            "uid": cl_uid, "name": cl_name, "path": cl_path,
            "line_number": cl_line, "docstring": cl_doc, "bases": cl_bases,
            "decorators": cl_dec, "kind": cl_kind,
        }), f"{pq_dir}/classes.parquet")

        pq.write_table(pa.table({
            "uid": v_uid, "name": v_name, "path": v_path,
            "line_number": v_line, "type": v_type,
        }), f"{pq_dir}/variables.parquet")

        if p_uid:
            pq.write_table(pa.table({
                "uid": p_uid, "name": p_name, "path": p_path,
                "function_uid": p_func_uid, "function_line_number": p_func_line,
            }), f"{pq_dir}/parameters.parquet")

        pq.write_table(pa.table({
            "file_path": im_fp, "module_name": im_mod, "imported_name": im_name,
            "alias": im_alias, "full_import_name": im_full, "line_number": im_line,
        }), f"{pq_dir}/imports.parquet")

        pq.write_table(pa.table({
            "file_path": fc_fp, "symbol_uid": fc_uid, "symbol_type": fc_type,
        }), f"{pq_dir}/file_contains.parquet")

        pq.write_table(pa.table({
            "caller_uid": c_caller, "called_uid": c_called,
            "caller_type": c_ct, "called_type": c_cdt,
            "caller_name": c_cn, "called_name": c_dn,
            "caller_path": c_cp, "called_path": c_dp,
            "line_number": c_ln, "full_call_name": c_fcn,
            "confidence": c_conf,
            "resolution_tier": pa.array(c_tier, type=pa.int32()),
            "receiver_type": c_recv,
        }), f"{pq_dir}/calls.parquet")

        if inh_child:
            pq.write_table(pa.table({
                "child_uid": inh_child, "parent_uid": inh_parent,
                "child_name": inh_cn, "parent_name": inh_pn,
                "child_path": inh_cp, "parent_path": inh_pp,
            }), f"{pq_dir}/inheritance.parquet")

        if on_progress:
            on_progress(total * 2 // 3, total, "Loading into DuckDB...")

        logger.info("  [timing] parquet_write: %.1fs", time.perf_counter() - _t_pq)

        # ── DROP + COPY FROM ─────────────────────────────────────────
        _t_db = time.perf_counter()
        c = self._conn

        # Drop existing data
        for tbl in ["operational_params", "rationales", "routes", "execution_flows", "calls", "inheritance", "injections", "orm_entities", "orm_repositories", "db_access", "file_contains",
                     "imports", "parameters", "variables", "classes", "functions",
                     "directories", "files", "modules", "repository"]:
            c.execute(f"DROP TABLE IF EXISTS {tbl}")
        self._schema_created = False
        self._create_schema()

        # Repository
        c.execute("INSERT INTO repository VALUES (?, ?)",
                  [str(repo_path_obj), repo_path_obj.name])

        # Bulk load from Parquet
        c.execute(f"INSERT INTO files SELECT * FROM read_parquet('{pq_dir}/files.parquet')")

        if os.path.exists(f"{pq_dir}/directories.parquet"):
            c.execute(f"INSERT INTO directories SELECT * FROM read_parquet('{pq_dir}/directories.parquet')")

        c.execute(f"INSERT INTO functions SELECT * FROM read_parquet('{pq_dir}/functions.parquet')")
        c.execute(f"INSERT INTO classes SELECT * FROM read_parquet('{pq_dir}/classes.parquet')")
        c.execute(f"INSERT INTO variables SELECT * FROM read_parquet('{pq_dir}/variables.parquet')")

        if os.path.exists(f"{pq_dir}/parameters.parquet"):
            c.execute(f"INSERT INTO parameters SELECT * FROM read_parquet('{pq_dir}/parameters.parquet')")

        # Modules (deduplicated)
        if mod_set:
            c.executemany("INSERT INTO modules VALUES (?)", [(m,) for m in mod_set])

        c.execute(f"INSERT INTO imports SELECT * FROM read_parquet('{pq_dir}/imports.parquet')")
        c.execute(f"INSERT INTO file_contains SELECT * FROM read_parquet('{pq_dir}/file_contains.parquet')")
        c.execute(f"INSERT INTO calls SELECT * FROM read_parquet('{pq_dir}/calls.parquet')")

        if os.path.exists(f"{pq_dir}/inheritance.parquet"):
            c.execute(f"INSERT INTO inheritance SELECT * FROM read_parquet('{pq_dir}/inheritance.parquet')")

        if inj_rows:
            c.executemany(
                "INSERT INTO injections VALUES (?,?,?,?,?,?,?,?)",
                _resolve_injection_targets(inj_rows, cl_name, cl_path),
            )

        if orm_rows:
            _write_orm(c, orm_rows)

        # Indexes for query performance
        c.execute("CREATE INDEX IF NOT EXISTS idx_fn_name ON functions(name)")
        c.execute("CREATE INDEX IF NOT EXISTS idx_fn_path ON functions(path)")
        c.execute("CREATE INDEX IF NOT EXISTS idx_fn_uid ON functions(uid)")
        c.execute("CREATE INDEX IF NOT EXISTS idx_cls_name ON classes(name)")
        c.execute("CREATE INDEX IF NOT EXISTS idx_cls_path ON classes(path)")
        c.execute("CREATE INDEX IF NOT EXISTS idx_var_path ON variables(path)")
        c.execute("CREATE INDEX IF NOT EXISTS idx_calls_caller ON calls(caller_uid)")
        c.execute("CREATE INDEX IF NOT EXISTS idx_calls_called ON calls(called_uid)")
        c.execute("CREATE INDEX IF NOT EXISTS idx_calls_types ON calls(caller_type, called_type)")
        c.execute("CREATE INDEX IF NOT EXISTS idx_fc_file ON file_contains(file_path)")
        c.execute("CREATE INDEX IF NOT EXISTS idx_fc_uid ON file_contains(symbol_uid)")
        c.execute("CREATE INDEX IF NOT EXISTS idx_imp_file ON imports(file_path)")

        logger.info("  [timing] duckdb_load: %.1fs", time.perf_counter() - _t_db)

        # ── Detect execution flows ────────────────────────────────
        _t = time.perf_counter()
        try:
            from ..execution_flows import detect_execution_flows
            import json as _json
            flows = detect_execution_flows(parsed_results, call_groups, repo_path=str(repo_path_obj))
            if flows:
                flow_rows = [
                    (f["name"], f["entry_file"], f["entry_line"],
                     f.get("entry_class", ""), f["step_count"],
                     f["depth"], f["score"], _json.dumps(f["steps"]))
                    for f in flows
                ]
                c.executemany(
                    "INSERT INTO execution_flows VALUES (?,?,?,?,?,?,?,?)",
                    flow_rows,
                )
        except Exception as exc:
            logger.debug("Execution flow detection failed: %s", exc)
            flows = []

        logger.info("  [timing] execution_flows: %.1fs", time.perf_counter() - _t)

        # ── Detect API routes ────────────────────────────────────
        _t = time.perf_counter()
        detected_routes = []
        try:
            from ..route_extraction import extract_routes
            detected_routes = extract_routes(parsed_results, repo_path)
            if detected_routes:
                c.executemany(
                    "INSERT INTO routes VALUES (?,?,?,?,?,?)",
                    [(r["method"], r["path"], r["handler"],
                      r["file"], r["line"], r["framework"])
                     for r in detected_routes],
                )
        except Exception as exc:
            logger.debug("Route extraction failed: %s", exc)

        logger.info("  [timing] route_extraction: %.1fs", time.perf_counter() - _t)

        # ── Extract design rationale comments ─────────────────────
        _t = time.perf_counter()
        detected_rationales = []
        try:
            from ..rationale_extraction import extract_rationales
            detected_rationales = extract_rationales(parsed_results, repo_path)
            if detected_rationales:
                c.executemany(
                    "INSERT INTO rationales VALUES (?,?,?,?,?)",
                    [(r["tag"], r["text"], r["file"], r["line"], r["context"])
                     for r in detected_rationales],
                )
        except Exception as exc:
            logger.debug("Rationale extraction failed: %s", exc)

        logger.info("  [timing] rationale_extraction: %.1fs", time.perf_counter() - _t)

        # ── Extract operational parameters ──────────────────────────
        _t = time.perf_counter()
        detected_op_params = []
        try:
            from ..op_param_extraction import extract_operational_params
            detected_op_params = extract_operational_params(parsed_results, repo_path)
            if detected_op_params:
                c.executemany(
                    "INSERT INTO operational_params VALUES (?,?,?,?,?)",
                    [(p["name"], p["value"], p["path"],
                      p["line_number"], p["category"])
                     for p in detected_op_params],
                )
        except Exception as exc:
            logger.debug("Operational param extraction failed: %s", exc)

        logger.info("  [timing] op_param_extraction: %.1fs", time.perf_counter() - _t)

        # ── Post-process: fill inheritance from classes.bases ──────
        try:
            # Build class name → uid map
            class_map = {}
            for row in c.execute("SELECT uid, name, path FROM classes").fetchall():
                class_map[row[1]] = (row[0], row[2])

            # Check existing inheritance count
            existing = c.execute("SELECT count(*) FROM inheritance").fetchone()[0]

            # For each class with bases, create inheritance edges if not exists
            import re as _re
            added = 0
            for row in c.execute("SELECT uid, name, path, bases FROM classes WHERE bases != ''").fetchall():
                child_uid, child_name, child_path, bases_str = row
                # Normalize: remove newlines, strip "extends"/"implements" keywords
                cleaned = bases_str.replace("\n", " ").replace("\r", "")
                cleaned = _re.sub(r"\b(extends|implements)\b", ",", cleaned)
                # Strip ALL generic type params: BaseCrudService<A, B, C> → BaseCrudService
                stripped = _re.sub(r"<[^<>]*>", "", cleaned)
                while "<" in stripped:
                    stripped = _re.sub(r"<[^<>]*>", "", stripped)
                # Split by comma, clean whitespace
                base_names = [b.strip() for b in stripped.split(",") if b.strip()]
                for base_name in base_names:
                    if base_name in class_map:
                        parent_uid, parent_path = class_map[base_name]
                        # Skip if the resolver already linked this child to a
                        # parent of that name (possibly in another file).
                        exists = c.execute(
                            "SELECT 1 FROM inheritance WHERE child_name=? AND child_path=? AND parent_name=?",
                            [child_name, child_path, base_name],
                        ).fetchone()
                        if not exists:
                            c.execute(
                                "INSERT INTO inheritance VALUES (?,?,?,?,?,?)",
                                [child_uid, parent_uid, child_name, base_name,
                                 child_path, parent_path],
                            )
                            added += 1
            if added:
                logger.info("Post-process: added %d inheritance edges (was %d)", added, existing)
        except Exception as exc:
            logger.debug("Inheritance post-process failed: %s", exc)

        # Cleanup temp parquet
        import shutil
        shutil.rmtree(pq_dir, ignore_errors=True)

        elapsed = time.perf_counter() - t_start

        if on_progress:
            on_progress(total, total, f"DuckDB write complete ({elapsed:.1f}s)")

        counts = {
            "files": len(f_path),
            "functions": len(fn_uid),
            "classes": len(cl_uid),
            "variables": len(v_uid),
            "parameters": len(p_uid),
            "calls": len(c_caller),
            "imports": len(im_fp),
            "modules": len(mod_set),
            "inheritance": len(inh_child),
            "execution_flows": len(flows),
            "routes": len(detected_routes),
            "rationales": len(detected_rationales),
            "operational_params": len(detected_op_params),
            "elapsed_s": round(elapsed, 2),
        }
        logger.info(f"DuckDB write complete: {counts}")
        return counts

    # ── Query methods (for CGCBridge) ────────────────────────────────

    def get_stats(self) -> Dict[str, int]:
        c = self._conn
        stats = {}
        for tbl in ["files", "functions", "classes", "variables", "calls", "modules", "imports"]:
            try:
                stats[tbl] = c.execute(f"SELECT count(*) FROM {tbl}").fetchone()[0]
            except Exception:
                stats[tbl] = 0
        return stats

    def get_operational_params(self, file_paths: List[str] = None) -> List[Dict]:
        """Get operational parameters, optionally filtered by file paths."""
        try:
            if file_paths:
                placeholders = ",".join(["?"] * len(file_paths))
                rows = self._conn.execute(f"""
                    SELECT name, value, path, line_number, category
                    FROM operational_params
                    WHERE path IN ({placeholders})
                    ORDER BY path, line_number
                """, file_paths).fetchall()
            else:
                rows = self._conn.execute("""
                    SELECT name, value, path, line_number, category
                    FROM operational_params
                    ORDER BY path, line_number
                """).fetchall()
            return [
                {"name": r[0], "value": r[1], "path": r[2],
                 "line_number": r[3], "category": r[4]}
                for r in rows
            ]
        except Exception:
            return []

    def get_top_connected(self, limit: int = 30) -> List[Dict]:
        """Get top connected functions/classes by call count."""
        rows = self._conn.execute("""
            SELECT
                called_name AS name,
                called_path AS path,
                called_type AS type,
                count(*) AS call_count
            FROM calls
            WHERE called_type IN ('Function', 'Class')
            GROUP BY called_name, called_path, called_type
            ORDER BY call_count DESC
            LIMIT ?
        """, [limit]).fetchall()
        return [
            {"name": r[0], "path": r[1], "type": r[2], "call_count": r[3]}
            for r in rows
        ]

    def get_call_graph_for_files(
        self, file_paths: List[str]
    ) -> Dict[str, List[Dict]]:
        """Get intra/outgoing/incoming call edges for given files."""
        if not file_paths:
            return {"intra": [], "outgoing": [], "incoming": []}

        placeholders = ",".join(["?"] * len(file_paths))

        # All edges where caller OR called is in our files
        rows = self._conn.execute(f"""
            SELECT caller_name, caller_path, caller_type,
                   called_name, called_path, called_type,
                   line_number, full_call_name
            FROM calls
            WHERE caller_path IN ({placeholders})
               OR called_path IN ({placeholders})
        """, file_paths + file_paths).fetchall()

        fp_set = set(file_paths)
        intra = []; outgoing = []; incoming = []

        for r in rows:
            edge = {
                "caller_name": r[0], "caller_path": r[1], "caller_type": r[2],
                "called_name": r[3], "called_path": r[4], "called_type": r[5],
                "line_number": r[6], "full_call_name": r[7],
            }
            caller_in = r[1] in fp_set
            called_in = r[4] in fp_set

            if caller_in and called_in:
                intra.append(edge)
            elif caller_in:
                outgoing.append(edge)
            elif called_in:
                incoming.append(edge)

        return {"intra": intra, "outgoing": outgoing, "incoming": incoming}

    def get_functions_in_file(self, file_path: str) -> List[Dict]:
        rows = self._conn.execute("""
            SELECT name, line_number, complexity, return_type, docstring, class_context, is_async
            FROM functions WHERE path = ?
            ORDER BY line_number
        """, [file_path]).fetchall()
        return [
            {"name": r[0], "line_number": r[1], "complexity": r[2],
             "return_type": r[3], "docstring": r[4], "class_context": r[5], "is_async": r[6]}
            for r in rows
        ]

    def get_classes_in_file(self, file_path: str) -> List[Dict]:
        rows = self._conn.execute("""
            SELECT name, line_number, docstring, bases
            FROM classes WHERE path = ?
            ORDER BY line_number
        """, [file_path]).fetchall()
        return [
            {"name": r[0], "line_number": r[1], "docstring": r[2], "bases": r[3]}
            for r in rows
        ]

    def get_imports_for_file(self, file_path: str) -> List[Dict]:
        rows = self._conn.execute("""
            SELECT module_name, imported_name, alias, line_number
            FROM imports WHERE file_path = ?
        """, [file_path]).fetchall()
        return [
            {"module_name": r[0], "imported_name": r[1], "alias": r[2], "line_number": r[3]}
            for r in rows
        ]

    def search_symbols(self, query: str, limit: int = 20) -> List[Dict]:
        """Search functions and classes by name pattern."""
        pattern = f"%{query}%"
        rows = self._conn.execute("""
            SELECT 'Function' AS type, name, path, line_number FROM functions WHERE name ILIKE ?
            UNION ALL
            SELECT 'Class' AS type, name, path, line_number FROM classes WHERE name ILIKE ?
            ORDER BY name LIMIT ?
        """, [pattern, pattern, limit]).fetchall()
        return [
            {"type": r[0], "name": r[1], "path": r[2], "line_number": r[3]}
            for r in rows
        ]

    def get_routes(self, limit: int = 100) -> List[Dict]:
        """Get detected API routes."""
        try:
            rows = self._conn.execute("""
                SELECT method, path, handler, file, line, framework
                FROM routes ORDER BY path, method LIMIT ?
            """, [limit]).fetchall()
            return [
                {"method": r[0], "path": r[1], "handler": r[2],
                 "file": r[3], "line": r[4], "framework": r[5]}
                for r in rows
            ]
        except Exception:
            return []

    def get_execution_flows(self, limit: int = 50) -> List[Dict]:
        """Get top execution flows by score."""
        import json
        try:
            rows = self._conn.execute("""
                SELECT name, entry_file, entry_line, entry_class,
                       step_count, depth, score, steps_json
                FROM execution_flows
                ORDER BY score DESC, step_count DESC
                LIMIT ?
            """, [limit]).fetchall()
            return [
                {
                    "name": r[0], "entry_file": r[1], "entry_line": r[2],
                    "entry_class": r[3], "step_count": r[4], "depth": r[5],
                    "score": r[6], "steps": json.loads(r[7]),
                }
                for r in rows
            ]
        except Exception:
            return []

    def get_rationales(self, limit: int = 200) -> List[Dict]:
        """Get design rationale comments."""
        try:
            rows = self._conn.execute("""
                SELECT tag, text, file, line, context
                FROM rationales ORDER BY file, line LIMIT ?
            """, [limit]).fetchall()
            return [
                {"tag": r[0], "text": r[1], "file": r[2],
                 "line": r[3], "context": r[4]}
                for r in rows
            ]
        except Exception:
            return []

    def execute(self, query: str, params=None):
        """Raw query execution for advanced use."""
        if params:
            return self._conn.execute(query, params)
        return self._conn.execute(query)


def _resolve_injection_targets(inj_rows, class_names, class_paths):
    """Attach the repo file declaring each injected type.

    Unique type name → that file; otherwise the candidate matching one of the
    injector file's imports; otherwise one in the injector's own package
    directory (Java same-package types need no import). Unresolvable or
    external types (e.g. a framework bean) keep an empty path.
    """
    by_name: Dict[str, List[str]] = {}
    for name, path in zip(class_names, class_paths):
        if name and path not in by_name.setdefault(name, []):
            by_name[name].append(path)
    out = []
    for injector, rel, typ, field, line, kind, stereotype, imports in inj_rows:
        cands = by_name.get(typ, [])
        target = ""
        if len(cands) == 1:
            target = cands[0]
        elif cands:
            for imp in imports:
                if imp.rsplit(".", 1)[-1] == typ:
                    imp_path = imp.replace(".", "/")
                    target = next((p for p in cands if imp_path in p), "")
                    if target:
                        break
            if not target:
                pkg_dir = os.path.dirname(rel)
                target = next((p for p in cands if os.path.dirname(p) == pkg_dir), "")
        out.append((injector, rel, typ, target, field, line, kind, stereotype))
    return out


# Spring Data CRUD / derived-query method prefixes (lower-case). Writes first:
# "saveAndFlush" must not match a read prefix.
_REPO_WRITE_PREFIXES = (
    "save", "insert", "update", "delete", "remove", "flush", "create", "upsert",
)
_REPO_READ_PREFIXES = (
    "find", "read", "get", "query", "search", "count", "exists", "stream", "fetch", "load",
)
_SPRING_DATA_BASES = {
    "JpaRepository", "CrudRepository", "PagingAndSortingRepository", "ListCrudRepository",
    "ListPagingAndSortingRepository", "Repository", "MongoRepository", "ReactiveMongoRepository",
    "CassandraRepository", "ReactiveCassandraRepository", "R2dbcRepository",
    "JpaSpecificationExecutor", "QuerydslPredicateExecutor", "CoroutineCrudRepository",
}


def _split_table(name: str) -> Tuple[str, str]:
    """`schema.table` -> (table, schema)."""
    schema, _, table = name.rpartition(".")
    return table, schema


def _write_orm(c, orm_rows) -> None:
    """Fill orm_entities, orm_repositories and db_access.

    db_access combines two sources:
    - query: tables named in a repository method's @Query / MyBatis SQL
      (JPQL entity names mapped to their tables);
    - repository: resolved calls whose receiver is a repository, e.g.
      petRepository.save(...) / findByOwner(...) -> the entity's table, with
      READ/WRITE from the Spring Data method name (or the method's @Query).
    """
    entities = {}  # class -> (path, table, schema, datastore, line)
    repos_raw = []
    queries = []
    for rel, m in orm_rows:
        kind = m.get("kind")
        if kind == "entity" and m.get("tables"):
            entities.setdefault(m["class_name"], (
                rel, m["tables"][0], m.get("schema") or "", m.get("datastore") or "", m.get("line_number", 0)))
        elif kind == "repository":
            repos_raw.append((rel, m))
        elif kind == "query":
            queries.append((rel, m))

    c.executemany("INSERT INTO orm_entities VALUES (?,?,?,?,?,?)", [
        (cls, p, table, schema, store, line) for cls, (p, table, schema, store, line) in entities.items()
    ])

    repos = {}  # repository -> (path, entity, table, schema)
    for rel, m in repos_raw:
        repo, base, ent = m["class_name"], m.get("base") or "", m.get("entity") or ""
        if ent not in entities or repo in repos:
            continue
        if base in _SPRING_DATA_BASES or base.endswith("Repository") or repo.endswith("Repository"):
            _, table, schema, _, _ = entities[ent]
            repos[repo] = (rel, ent, table, schema)
    c.executemany("INSERT INTO orm_repositories VALUES (?,?,?,?,?,?)", [
        (repo, p, ent, entities[ent][0], table, schema) for repo, (p, ent, table, schema) in repos.items()
    ])

    def query_tables(m, repo_entity=None):
        out = []
        for t in m.get("tables") or []:
            if m.get("native"):
                table, schema = _split_table(t)
                out.append((table, schema))
            elif t in entities:  # JPQL entity name
                out.append((entities[t][1], entities[t][2]))
            elif t == "entityName" and repo_entity in entities:  # SpEL #{#entityName}
                out.append((entities[repo_entity][1], entities[repo_entity][2]))
        return out

    # Repository -> its generic base interface (for queries declared on a
    # shared base such as TenantAwareRepository<E>)
    repo_base = {m["class_name"]: m.get("base") or "" for _, m in repos_raw}
    queries_by_key = {(m["class_name"], m.get("method_name") or ""): m for _, m in queries}

    access = []
    repo_queries = {}  # (repository, method) -> [(table, schema, op)]
    for rel, m in queries:
        op = m.get("operation") or "READ"
        tables = query_tables(m)
        repo_queries[(m["class_name"], m.get("method_name") or "")] = [(t, s, op) for t, s in tables]
        for table, schema in tables:
            access.append((m["class_name"], m.get("method_name") or "", rel, m.get("line_number", 0),
                           table, schema, op, "query", m["class_name"], m.get("method_name") or ""))

    if repos:
        names = list(repos)
        rows = c.execute(
            "SELECT caller_name, caller_path, line_number, called_name, receiver_type FROM calls "
            "WHERE confidence <> 'AMBIGUOUS' AND receiver_type IN (SELECT unnest(?))", [names]
        ).fetchall()
        for caller, caller_path, line, method, repo in rows:
            declared = repo_queries.get((repo, method))
            base_query = queries_by_key.get((repo_base.get(repo, ""), method))
            if not declared and base_query:
                op = base_query.get("operation") or "READ"
                declared = [(t, s, op) for t, s in query_tables(base_query, repos[repo][1])]
            if declared:
                for table, schema, op in declared:
                    access.append(("", caller or "", caller_path, line, table, schema, op, "repository", repo, method))
                continue
            low = (method or "").lower()
            if low.startswith(_REPO_WRITE_PREFIXES):
                op = "WRITE"
            elif low.startswith(_REPO_READ_PREFIXES):
                op = "READ"
            else:
                continue
            _, _, table, schema = repos[repo]
            access.append(("", caller or "", caller_path, line, table, schema, op, "repository", repo, method))

    if access:
        c.executemany("INSERT INTO db_access VALUES (?,?,?,?,?,?,?,?,?,?)", list(dict.fromkeys(access)))
