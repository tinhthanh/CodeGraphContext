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
            body_start_line INTEGER DEFAULT 0, body_end_line INTEGER DEFAULT 0)""")
        c.execute("""CREATE TABLE IF NOT EXISTS classes (
            uid VARCHAR PRIMARY KEY, name VARCHAR, path VARCHAR,
            line_number INTEGER, docstring VARCHAR DEFAULT '',
            bases VARCHAR DEFAULT '')""")
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
            line_number INTEGER DEFAULT 0, full_call_name VARCHAR DEFAULT '')""")
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
        c.execute("""CREATE TABLE IF NOT EXISTS routes (
            method VARCHAR, path VARCHAR, handler VARCHAR,
            file VARCHAR, line INTEGER DEFAULT 0,
            framework VARCHAR DEFAULT '')""")
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
        fn_bstart = []; fn_bend = []
        # Classes
        cl_uid = []; cl_name = []; cl_path = []; cl_line = []; cl_doc = []; cl_bases = []
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

            # Functions
            for fn in r.get("functions", []):
                uid = f"{fn.get('name', '')}|{fp}|{fn.get('line_number', 0)}"
                if uid not in func_set:
                    func_set.add(uid)
                    fn_uid.append(uid); fn_name.append(fn.get("name", "")); fn_path.append(fp)
                    fn_line.append(fn.get("line_number", 0))
                    fn_cx.append(fn.get("complexity", 0) or 0)
                    fn_rt.append(fn.get("return_type", "") or "")
                    fn_doc.append(fn.get("docstring", "") or "")
                    fn_cc.append(fn.get("class_context", "") or "")
                    fn_async.append(fn.get("is_async", False) or False)
                    fn_bstart.append(fn.get("body_start_line", 0) or 0)
                    fn_bend.append(fn.get("body_end_line", 0) or 0)
                    fc_fp.append(fp); fc_uid.append(uid); fc_type.append("Function")

                    # Parameters
                    for arg in fn.get("args", []) or []:
                        arg_name = arg if isinstance(arg, str) else str(arg)
                        if arg_name:
                            puid = f"{arg_name}|{fp}|{fn.get('line_number', 0)}"
                            if puid not in param_set:
                                param_set.add(puid)
                                p_uid.append(puid); p_name.append(arg_name)
                                p_path.append(fp); p_func_uid.append(uid)
                                p_func_line.append(fn.get("line_number", 0))

            # Classes
            for cls in r.get("classes", []):
                uid = f"{cls.get('name', '')}|{fp}|{cls.get('line_number', 0)}"
                if uid not in class_set:
                    class_set.add(uid)
                    cl_uid.append(uid); cl_name.append(cls.get("name", ""))
                    cl_path.append(fp); cl_line.append(cls.get("line_number", 0))
                    cl_doc.append(cls.get("docstring", "") or "")
                    bases = cls.get("bases", [])
                    cl_bases.append(",".join(str(b) for b in bases) if bases else "")
                    fc_fp.append(fp); fc_uid.append(uid); fc_type.append("Class")

            # Variables
            for var in r.get("variables", []):
                uid = f"{var.get('name', '')}|{fp}|{var.get('line_number', 0)}"
                if uid not in var_set:
                    var_set.add(uid)
                    v_uid.append(uid); v_name.append(var.get("name", ""))
                    v_path.append(fp); v_line.append(var.get("line_number", 0))
                    v_type.append(var.get("type", "") or "")
                    fc_fp.append(fp); fc_uid.append(uid); fc_type.append("Variable")

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
        c_ln = []; c_fcn = []
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
                c_cp.append(e.get("caller_file_path", "")); c_dp.append(e.get("called_file_path", ""))
                c_ln.append(e.get("line_number", 0)); c_fcn.append(e.get("full_call_name", ""))

        # ── Inheritance ──────────────────────────────────────────────
        inh_child = []; inh_parent = []; inh_cn = []; inh_pn = []; inh_cp = []; inh_pp = []
        for edge in (inheritance or []):
            child_name = edge.get("child_name", "")
            parent_name = edge.get("parent_name", "")
            child_path = edge.get("child_file_path", "")
            parent_path = edge.get("parent_file_path", "")
            inh_child.append(f"{child_name}|{child_path}|0")
            inh_parent.append(f"{parent_name}|{parent_path}|0")
            inh_cn.append(child_name); inh_pn.append(parent_name)
            inh_cp.append(child_path); inh_pp.append(parent_path)

        # ── Write Parquet ────────────────────────────────────────────
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
        }), f"{pq_dir}/functions.parquet")

        pq.write_table(pa.table({
            "uid": cl_uid, "name": cl_name, "path": cl_path,
            "line_number": cl_line, "docstring": cl_doc, "bases": cl_bases,
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
        }), f"{pq_dir}/calls.parquet")

        if inh_child:
            pq.write_table(pa.table({
                "child_uid": inh_child, "parent_uid": inh_parent,
                "child_name": inh_cn, "parent_name": inh_pn,
                "child_path": inh_cp, "parent_path": inh_pp,
            }), f"{pq_dir}/inheritance.parquet")

        if on_progress:
            on_progress(total * 2 // 3, total, "Loading into DuckDB...")

        # ── DROP + COPY FROM ─────────────────────────────────────────
        c = self._conn

        # Drop existing data
        for tbl in ["routes", "execution_flows", "calls", "inheritance", "file_contains",
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

        # ── Detect execution flows ────────────────────────────────
        try:
            from ..execution_flows import detect_execution_flows
            import json as _json
            flows = detect_execution_flows(parsed_results, call_groups)
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

        # ── Detect API routes ────────────────────────────────────
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

    def execute(self, query: str, params=None):
        """Raw query execution for advanced use."""
        if params:
            return self._conn.execute(query, params)
        return self._conn.execute(query)
