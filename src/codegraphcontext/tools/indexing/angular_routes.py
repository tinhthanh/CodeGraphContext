"""Angular Router: route tables, lazy-loaded modules and navigation calls.

Routes come from `Routes` arrays passed to `RouterModule.forRoot/forChild`
or `provideRouter`, or exported for `loadChildren`. Full paths are built by
following `loadChildren: () => import('./x/x.module').then(m => m.XModule)`
from the root routes into the child module's routing file, so a page gets
`/home/tabs/tab1` rather than just `tab1`.

Navigations are `router.navigate([...])` / `router.navigateByUrl('...')`
calls, attributed to their enclosing function and matched to a route.
"""

from __future__ import annotations

import os
import re
from typing import Any, Dict, List, Optional, Tuple

_ROUTE_ARRAY_DECL = re.compile(
    r"\b(?:export\s+)?(?:const|let|var)\s+(\w+)\s*(?::\s*(Routes|Route\s*\[\s*\]))?\s*=\s*\["
)
_ROUTER_CALL = re.compile(r"\b(?:RouterModule\.(forRoot|forChild)|(provideRouter))\s*\(")
_IMPORT_SPEC = re.compile(r"import\(\s*['\"]([^'\"]+)['\"]\s*\)")
_THEN_SYMBOL = re.compile(r"\.then\(\s*\(?\s*(\w+)\s*\)?\s*=>\s*\1\.(\w+)")
_NAVIGATE = re.compile(r"\b(?:\w+\.)*\w*[Rr]outer\s*\.\s*(navigate|navigateByUrl)\s*\(")
_TS_EXTS = (".ts", ".tsx", ".js", ".mjs")


# ── Small JS literal parser ──────────────────────────────────────────

def _strip_comments(src: str) -> str:
    """Blank out // and /* */ comments (string-aware, keeps offsets)."""
    out, i, n = [], 0, len(src)
    while i < n:
        c = src[i]
        if c in "'\"`":
            j = i + 1
            while j < n and src[j] != c:
                j += 2 if src[j] == "\\" else 1
            out.append(src[i:j + 1]); i = j + 1
        elif src.startswith("//", i):
            j = src.find("\n", i); j = n if j == -1 else j
            out.append(" " * (j - i)); i = j
        elif src.startswith("/*", i):
            j = src.find("*/", i + 2); j = n if j == -1 else j + 2
            out.append(re.sub(r"[^\n]", " ", src[i:j])); i = j
        else:
            out.append(c); i += 1
    return "".join(out)


def _match_close(code: str, i: int) -> int:
    """Index just past the bracket matching code[i] ('[', '{' or '(')."""
    depth, n = 0, len(code)
    while i < n:
        c = code[i]
        if c in "'\"`":
            j = i + 1
            while j < n and code[j] != c:
                j += 2 if code[j] == "\\" else 1
            i = j + 1
            continue
        if c in "[{(":
            depth += 1
        elif c in "]})":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def _split_top(code: str, start: int, end: int) -> List[Tuple[int, int]]:
    """Comma-separated top-level segments of code[start:end]."""
    segs, depth, seg_start, i = [], 0, start, start
    while i < end:
        c = code[i]
        if c in "'\"`":
            j = i + 1
            while j < end and code[j] != c:
                j += 2 if code[j] == "\\" else 1
            i = j + 1
            continue
        if c in "[{(":
            depth += 1
        elif c in "]})":
            depth -= 1
        elif c == "," and depth == 0:
            segs.append((seg_start, i)); seg_start = i + 1
        i += 1
    segs.append((seg_start, end))
    return [(a, b) for a, b in segs if code[a:b].strip()]


def _string_value(text: str) -> Optional[str]:
    m = re.fullmatch(r"\s*(['\"`])(.*?)\1\s*", text, re.S)
    return m.group(2) if m else None


def _parse_object(code: str, start: int, end: int) -> Dict[str, Tuple[str, int]]:
    """Top-level `key: value` pairs of an object literal code[start:end] ({...})."""
    props = {}
    for a, b in _split_top(code, start + 1, end - 1):
        m = re.match(r"\s*['\"]?(\w+)['\"]?\s*:", code[a:b])
        if m:
            vstart = a + m.end()
            props[m.group(1)] = (code[vstart:b].strip(), vstart + (len(code[vstart:b]) - len(code[vstart:b].lstrip())))
    return props


def _parse_routes(code: str, start: int, end: int) -> List[Dict[str, Any]]:
    """Parse a routes array code[start:end] ([...]) into route dicts."""
    routes = []
    for a, b in _split_top(code, start + 1, end - 1):
        seg = code[a:b].strip()
        off = a + (len(code[a:b]) - len(code[a:b].lstrip()))
        if seg.startswith("..."):  # spread of another routes array
            routes.append({"ref": seg[3:].strip(), "pos": off})
            continue
        if re.fullmatch(r"\w+", seg):
            routes.append({"ref": seg, "pos": off})
            continue
        if not seg.startswith("{"):
            continue
        props = _parse_object(code, off, _match_close(code, off))
        route: Dict[str, Any] = {"pos": off, "path": None}
        if "path" in props:
            route["path"] = _string_value(props["path"][0])
        if "component" in props:
            route["component"] = props["component"][0].strip()
        if "redirectTo" in props:
            route["redirect"] = _string_value(props["redirectTo"][0]) or props["redirectTo"][0]
        for key in ("loadChildren", "loadComponent"):
            if key in props:
                val = props[key][0]
                spec = _IMPORT_SPEC.search(val)
                sym = _THEN_SYMBOL.search(val)
                legacy = _string_value(val)  # 'path/to/x.module#XModule'
                if spec:
                    route[key] = (spec.group(1), sym.group(2) if sym else None)
                elif legacy and "#" in legacy:
                    p, s = legacy.split("#", 1)
                    route[key] = (p, s)
        if "children" in props and props["children"][0].startswith("["):
            cstart = props["children"][1]
            route["children"] = _parse_routes(code, cstart, _match_close(code, cstart))
        routes.append(route)
    return routes


# ── Per-file route tables ────────────────────────────────────────────

def _line_of(code: str, pos: int) -> int:
    return code.count("\n", 0, pos) + 1


def _file_routes(code: str) -> Dict[str, Any]:
    """Named route arrays and router registrations found in one file."""
    arrays: Dict[str, Dict[str, Any]] = {}
    for m in _ROUTE_ARRAY_DECL.finditer(code):
        start = m.end() - 1
        arrays[m.group(1)] = {
            "routes": _parse_routes(code, start, _match_close(code, start)),
            "typed": bool(m.group(2)),
        }
    registrations = []  # (mode, array name or None, inline routes or None)
    for m in _ROUTER_CALL.finditer(code):
        mode = "root" if (m.group(1) == "forRoot" or m.group(2)) else "child"
        args_start = m.end()
        while args_start < len(code) and code[args_start].isspace():
            args_start += 1
        if args_start < len(code) and code[args_start] == "[":
            registrations.append((mode, None, _parse_routes(code, args_start, _match_close(code, args_start))))
        else:
            ident = re.match(r"(\w+)", code[args_start:])
            if ident:
                registrations.append((mode, ident.group(1), None))
    return {"arrays": arrays, "registrations": registrations}


def _resolve_import(from_rel: str, spec: str, known: set) -> Optional[str]:
    if not spec.startswith("."):
        return None
    base = os.path.normpath(os.path.join(os.path.dirname(from_rel), spec))
    for cand in [base] + [base + e for e in _TS_EXTS] + [os.path.join(base, "index" + e) for e in _TS_EXTS]:
        if cand in known:
            return cand
    return None


def _join(prefix: str, path: Optional[str]) -> str:
    # `path: \`${PREFIX}/x\`` → dynamic segment
    path = re.sub(r"\$\{[^}]*\}", ":param", path or "")
    parts = [p for p in (prefix.strip("/"), path.strip("/")) if p]
    return "/" + "/".join(parts)


def extract_angular_routes(
    parsed_results: List[Dict[str, Any]],
    repo_path: str,
) -> List[Dict[str, Any]]:
    """Angular routes with full paths, as route dicts (method PAGE/REDIRECT)."""
    files: Dict[str, Dict[str, Any]] = {}
    codes: Dict[str, str] = {}
    for fd in parsed_results:
        path = fd.get("path", "")
        if fd.get("lang") not in ("typescript", "tsx", "javascript") or not os.path.exists(path):
            continue
        try:
            src = open(path, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        if "Route" not in src and "provideRouter" not in src:
            continue
        rel = os.path.relpath(path, repo_path)
        code = _strip_comments(src)
        info = _file_routes(code)
        if info["arrays"] or info["registrations"]:
            files[rel] = info
            codes[rel] = code
    if not files:
        return []
    known = {os.path.relpath(fd.get("path", ""), repo_path) for fd in parsed_results}

    def array_by_name(rel: str, name: str):
        arr = files.get(rel, {}).get("arrays", {}).get(name)
        return arr["routes"] if arr else None

    def child_tables(target: str, symbol: Optional[str]) -> List[Tuple[str, List]]:
        """Route tables a lazy `loadChildren` target contributes."""
        out = []
        if target in files:
            if symbol and array_by_name(target, symbol) is not None:
                return [(target, array_by_name(target, symbol))]
            out += [(target, routes) for _, routes in _registered(target)]
        if not out:  # the module imports a sibling *-routing.module
            d = os.path.dirname(target)
            for rel in files:
                if os.path.dirname(rel) == d and rel != target and ("routing" in rel or "routes" in rel):
                    out += [(rel, routes) for mode, routes in _registered(rel) if mode == "child"]
        return out

    def _registered(rel: str) -> List[Tuple[str, List]]:
        info = files[rel]
        out = []
        for mode, name, inline in info["registrations"]:
            routes = inline if inline is not None else array_by_name(rel, name) if name else None
            if routes is not None:
                out.append((mode, routes))
        return out

    results: List[Dict[str, Any]] = []
    emitted = set()
    reached = set()  # ids of route lists visited from a root

    def walk(rel: str, routes: List, prefix: str, depth: int):
        if depth > 12:
            return
        reached.add(id(routes))
        for r in routes:
            if "ref" in r:  # ...OTHER_ROUTES / OTHER_ROUTES inside the array
                ref = array_by_name(rel, r["ref"])
                if ref is not None and id(ref) not in reached:
                    walk(rel, ref, prefix, depth + 1)
                continue
            full = _join(prefix, r.get("path"))
            line = _line_of(codes[rel], r["pos"])
            handler, method = None, "PAGE"
            if r.get("component"):
                handler = r["component"]
            elif r.get("loadComponent"):
                handler = r["loadComponent"][1] or r["loadComponent"][0]
            elif r.get("loadChildren"):
                handler = r["loadChildren"][1] or r["loadChildren"][0]
            elif r.get("redirect") is not None:
                to = str(r["redirect"])
                handler = "→ " + (to if to.startswith("/") else _join(prefix, to))
                method = "REDIRECT"
            if handler and (method, full, rel, line) not in emitted:
                emitted.add((method, full, rel, line))
                results.append({"method": method, "path": full, "handler": handler,
                                "file": rel, "line": line, "framework": "angular"})
            if r.get("children"):
                walk(rel, r["children"], full, depth + 1)
            if r.get("loadChildren"):
                spec, symbol = r["loadChildren"]
                target = _resolve_import(rel, spec, known)
                if target:
                    for trel, troutes in child_tables(target, symbol):
                        if id(troutes) not in reached:
                            walk(trel, troutes, full, depth + 1)

    for rel in files:
        for mode, routes in _registered(rel):
            if mode == "root":
                walk(rel, routes, "", 0)
    # Child tables never reached from a root (unknown mount point): keep
    # their local paths so the pages are still listed.
    for rel in files:
        for mode, routes in _registered(rel):
            if id(routes) not in reached:
                walk(rel, routes, "", 0)
    return results


# ── Navigations ──────────────────────────────────────────────────────

def _nav_target(code: str, args_start: int, kind: str) -> Optional[str]:
    """Target path of a navigate([...]) / navigateByUrl('...') call.

    Non-literal segments become `:param`; a path is absolute when its first
    literal segment starts with '/'.
    """
    while args_start < len(code) and code[args_start].isspace():
        args_start += 1
    if kind == "navigate":
        if args_start >= len(code) or code[args_start] != "[":
            return None
        end = _match_close(code, args_start)
        parts, absolute = [], False
        for idx, (a, b) in enumerate(_split_top(code, args_start + 1, end - 1)):
            s = _string_value(code[a:b])
            if s is None:
                parts.append(":param")
                continue
            if idx == 0 and s.startswith("/"):
                absolute = True
            parts += [":param" if "${" in p else p for p in s.split("/") if p]
        return ("/" if absolute else "") + "/".join(parts)
    m = re.match(r"(['\"`])(.*?)\1", code[args_start:args_start + 500], re.S)
    if not m:
        return None
    return re.sub(r"\$\{[^}]*\}", ":param", m.group(2).split("?")[0])


def _match_route(target: str, routes: List[Dict[str, Any]]) -> Optional[Dict[str, Any]]:
    """Best route for an absolute target path (literal segments must match)."""
    if not target.startswith("/"):
        return None
    tseg = [s for s in target.strip("/").split("/") if s]
    best, best_score = None, -1
    for r in routes:
        if r["method"] != "PAGE":
            continue
        rseg = [s for s in r["path"].strip("/").split("/") if s]
        if len(rseg) != len(tseg):
            continue
        score = 0
        for a, b in zip(rseg, tseg):
            if a.startswith(":") or b == ":param":
                continue
            if a != b:
                break
            score += 1
        else:
            if score > best_score:
                best, best_score = r, score
    return best


def extract_navigations(
    parsed_results: List[Dict[str, Any]],
    repo_path: str,
    angular_routes: List[Dict[str, Any]],
) -> List[Dict[str, Any]]:
    """router.navigate / navigateByUrl calls with their enclosing function and route."""
    out = []
    for fd in parsed_results:
        path = fd.get("path", "")
        if fd.get("lang") not in ("typescript", "tsx", "javascript") or not os.path.exists(path):
            continue
        try:
            src = open(path, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        if "navigate" not in src:
            continue
        code = _strip_comments(src)
        rel = os.path.relpath(path, repo_path)
        funcs = [(f.get("line_number", 0), f.get("end_line", 0) or f.get("line_number", 0), f.get("name", ""))
                 for f in fd.get("functions", [])]
        for m in _NAVIGATE.finditer(code):
            target = _nav_target(code, m.end(), m.group(1))
            if target is None:
                continue
            line = _line_of(code, m.start())
            enclosing = [f for f in funcs if f[0] <= line <= f[1]]
            caller = max(enclosing, key=lambda f: f[0])[2] if enclosing else ""
            route = _match_route(target, angular_routes)
            out.append({
                "caller_name": caller, "caller_path": rel, "line": line, "target": target,
                "route_path": route["path"] if route else "",
                "route_handler": route["handler"] if route else "",
                "route_file": route["file"] if route else "",
            })
    return out
