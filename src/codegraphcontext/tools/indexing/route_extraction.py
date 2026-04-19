"""Detect API routes/endpoints from parsed source code.

Supports:
- Express.js/Hono: app.get("/path", handler), router.post(...)
- FastAPI/Flask: @app.get("/path"), @app.route("/path")
- Spring Boot: @GetMapping("/path"), @PostMapping, @RequestMapping
- Next.js: file-based routing (page.tsx → route)
- Django: path("url/", view), urlpatterns
- NestJS: @Get(), @Post(), @Controller("/prefix")
- Laravel: Route::get("/path", [Controller, "method"])
- Go: http.HandleFunc("/path", handler), r.GET("/path", handler)
"""

from __future__ import annotations

import logging
import os
import re
from pathlib import Path
from typing import Any, Dict, List, Optional

logger = logging.getLogger(__name__)

# ── Route patterns per framework ────────────────────────────────────

# Decorator-based routes (Python, Java, NestJS)
_DECORATOR_ROUTE_PATTERNS = [
    # FastAPI / Flask
    re.compile(r'@(?:app|router|api)\.(get|post|put|delete|patch|options|head)\s*\(\s*["\']([^"\']+)["\']'),
    re.compile(r'@(?:app|router|api)\.route\s*\(\s*["\']([^"\']+)["\']'),
    # Spring Boot
    re.compile(r'@(Get|Post|Put|Delete|Patch)Mapping\s*\(\s*(?:value\s*=\s*)?["\']([^"\']+)["\']'),
    re.compile(r'@RequestMapping\s*\(\s*(?:value\s*=\s*)?["\']([^"\']+)["\']'),
    # NestJS
    re.compile(r'@(Get|Post|Put|Delete|Patch)\s*\(\s*["\']([^"\']+)["\']'),
    re.compile(r'@Controller\s*\(\s*["\']([^"\']+)["\']'),
]

# Function call-based routes (Express, Hono, Go)
_CALL_ROUTE_PATTERNS = [
    # Express.js / Hono
    re.compile(r'(?:app|router|server|api)\.(get|post|put|delete|patch|use|all)\s*\(\s*["\']([^"\']+)["\']'),
    # Go net/http
    re.compile(r'(?:http\.)?HandleFunc\s*\(\s*["\']([^"\']+)["\']'),
    # Go gin/echo/chi
    re.compile(r'(?:r|router|e|g)\.(GET|POST|PUT|DELETE|PATCH)\s*\(\s*["\']([^"\']+)["\']'),
    # Laravel
    re.compile(r'Route::(get|post|put|delete|patch)\s*\(\s*["\']([^"\']+)["\']'),
    # Django
    re.compile(r'path\s*\(\s*["\']([^"\']+)["\']'),
]

# Next.js / Nuxt file-based routing patterns
_NEXTJS_ROUTE_FILES = {"page.tsx", "page.ts", "page.jsx", "page.js", "route.tsx", "route.ts"}
_NEXTJS_ROUTE_GROUPS = re.compile(r'\(([^)]+)\)')  # strip (group) from path


def _extract_nextjs_route(file_path: str, repo_path: str) -> Optional[str]:
    """Convert Next.js file path to route path."""
    try:
        rel = os.path.relpath(file_path, repo_path)
    except ValueError:
        return None

    parts = Path(rel).parts
    # Find "app" directory
    try:
        app_idx = list(parts).index("app")
    except ValueError:
        return None

    route_parts = []
    for part in parts[app_idx + 1:-1]:  # skip "app" and filename
        # Strip route groups like (dashboard), (auth)
        if part.startswith("(") and part.endswith(")"):
            continue
        # Convert [param] to :param
        if part.startswith("[") and part.endswith("]"):
            param = part[1:-1]
            if param.startswith("..."):
                route_parts.append(f"*{param[3:]}")
            else:
                route_parts.append(f":{param}")
        else:
            route_parts.append(part)

    return "/" + "/".join(route_parts) if route_parts else "/"


def extract_routes(
    parsed_results: List[Dict[str, Any]],
    repo_path: str,
) -> List[Dict[str, Any]]:
    """Extract API routes from parsed source files.

    Returns list of:
        {
            method: str (GET, POST, etc.),
            path: str ("/api/users/:id"),
            handler: str (function name),
            file: str (relative path),
            line: int,
            framework: str ("express", "fastapi", "spring", "nextjs", etc.),
        }
    """
    routes: List[Dict[str, Any]] = []
    seen: set = set()

    for file_data in parsed_results:
        if "error" in file_data:
            continue

        file_path = file_data.get("path", "")
        file_name = Path(file_path).name
        lang = file_data.get("lang", "")

        try:
            rel_path = os.path.relpath(file_path, repo_path)
        except ValueError:
            rel_path = file_name

        # ── Next.js file-based routing ──
        if file_name in _NEXTJS_ROUTE_FILES:
            route = _extract_nextjs_route(file_path, repo_path)
            if route is not None:
                # Next.js convention: route handler is ALWAYS `export default`.
                # Scan source for that pattern first — definitive, avoids picking
                # private helper functions like ParentRow, StatsCard, etc.
                handler = ""
                if os.path.exists(file_path):
                    try:
                        with open(file_path, "r", encoding="utf-8", errors="replace") as fh:
                            src = fh.read()
                        # export default function Name(...)
                        m = re.search(r'export\s+default\s+(?:async\s+)?function\s+([A-Z]\w*)', src)
                        if m:
                            handler = m.group(1)
                        else:
                            # export default Name    |    export default const Name = ...
                            m = re.search(r'export\s+default\s+([A-Z]\w*)\b', src)
                            if m:
                                handler = m.group(1)
                    except OSError:
                        pass

                if not handler:
                    for fn in file_data.get("functions", []):
                        name = fn.get("name", "")
                        if name and name[0].isupper():
                            handler = name
                            break
                if not handler:
                    fns = file_data.get("functions", [])
                    handler = fns[0]["name"] if fns else file_name

                key = f"GET|{route}"
                if key not in seen:
                    seen.add(key)
                    routes.append({
                        "method": "GET",
                        "path": route,
                        "handler": handler,
                        "file": rel_path,
                        "line": 0,
                        "framework": "nextjs",
                    })

        # ── Decorator-based routes (read from decorators on functions) ──
        for fn in file_data.get("functions", []):
            decorators = fn.get("decorators", []) or []
            for dec in decorators:
                dec_str = str(dec)
                for pattern in _DECORATOR_ROUTE_PATTERNS:
                    m = pattern.search(dec_str)
                    if m:
                        groups = m.groups()
                        if len(groups) == 2:
                            method = groups[0].upper()
                            path = groups[1]
                        else:
                            method = "ANY"
                            path = groups[0]

                        # Normalize method
                        method_map = {"GET": "GET", "POST": "POST", "PUT": "PUT",
                                      "DELETE": "DELETE", "PATCH": "PATCH",
                                      "GETMAPPING": "GET", "POSTMAPPING": "POST",
                                      "PUTMAPPING": "PUT", "DELETEMAPPING": "DELETE",
                                      "PATCHMAPPING": "PATCH"}
                        method = method_map.get(method.upper().replace("MAPPING", "MAPPING"), method)

                        framework = "spring" if "Mapping" in dec_str else \
                                    "fastapi" if lang == "python" else \
                                    "nestjs" if lang in ("typescript", "javascript") else "unknown"

                        key = f"{method}|{path}"
                        if key not in seen:
                            seen.add(key)
                            routes.append({
                                "method": method,
                                "path": path,
                                "handler": fn.get("name", ""),
                                "file": rel_path,
                                "line": fn.get("line_number", 0),
                                "framework": framework,
                            })
                        break

        # ── Java/Kotlin annotation-based routes (source scan fallback) ──
        # Rust parser doesn't extract Java annotations as decorators,
        # so scan source lines for @GetMapping, @PostMapping, etc.
        if lang in ("java", "kotlin") and os.path.exists(file_path):
            try:
                with open(file_path, "r", encoding="utf-8", errors="replace") as fh:
                    source_lines = fh.readlines()

                # Find class-level @RequestMapping prefix
                class_prefix = ""
                for line in source_lines:
                    rm = re.search(r'@RequestMapping\s*\(\s*(?:value\s*=\s*)?["\']([^"\']+)["\']', line)
                    if rm:
                        class_prefix = rm.group(1).rstrip("/")
                        break

                # Find method-level mappings
                annotation_re = re.compile(
                    r'@(Get|Post|Put|Delete|Patch)Mapping\s*\(\s*(?:value\s*=\s*)?["\']([^"\']+)["\']'
                )
                for i, line in enumerate(source_lines):
                    am = annotation_re.search(line)
                    if am:
                        method = am.group(1).upper()
                        path = class_prefix + am.group(2)
                        if not path.startswith("/"):
                            path = "/" + path

                        # Find handler: closest function AFTER this annotation (within 5 lines)
                        handler = ""
                        ann_line = i + 1
                        best_fn = None
                        best_dist = 999
                        for fn in file_data.get("functions", []):
                            fn_line = fn.get("line_number", 0)
                            if fn_line >= ann_line and (fn_line - ann_line) < best_dist:
                                best_dist = fn_line - ann_line
                                best_fn = fn
                        if best_fn and best_dist <= 5:
                            handler = best_fn.get("name", "")

                        key = f"{method}|{path}"
                        if key not in seen:
                            seen.add(key)
                            routes.append({
                                "method": method,
                                "path": path,
                                "handler": handler,
                                "file": rel_path,
                                "line": i + 1,
                                "framework": "spring",
                            })
            except OSError:
                pass

        # ── NestJS decorator-based routes (source scan) ──
        if lang in ("typescript", "javascript") and os.path.exists(file_path):
            try:
                with open(file_path, "r", encoding="utf-8", errors="replace") as fh:
                    source_lines = fh.readlines()

                # Check if this is a NestJS controller
                is_nestjs = any("@Controller" in line for line in source_lines)
                if is_nestjs:
                    # Find controller prefix
                    ctrl_prefix = ""
                    for line in source_lines:
                        cm = re.search(r"@Controller\s*\(\s*['\"]([^'\"]+)['\"]", line)
                        if cm:
                            ctrl_prefix = "/" + cm.group(1).strip("/")
                            break

                    # Find route decorators
                    nestjs_re = re.compile(
                        r"@(Get|Post|Put|Delete|Patch)\s*\(\s*(?:['\"]([^'\"]*)['\"])?\s*\)"
                    )
                    for i, line in enumerate(source_lines):
                        nm = nestjs_re.search(line)
                        if nm:
                            method = nm.group(1).upper()
                            path_suffix = nm.group(2) or ""
                            path = ctrl_prefix
                            if path_suffix:
                                path = ctrl_prefix + "/" + path_suffix.strip("/")
                            if not path:
                                path = "/"

                            # Find handler: closest function after annotation (within 5 lines)
                            handler = ""
                            ann_line = i + 1
                            best_fn = None
                            best_dist = 999
                            for fn in file_data.get("functions", []):
                                fn_line = fn.get("line_number", 0)
                                if fn_line >= ann_line and (fn_line - ann_line) < best_dist:
                                    best_dist = fn_line - ann_line
                                    best_fn = fn
                            if best_fn and best_dist <= 5:
                                handler = best_fn.get("name", "")

                            key = f"{method}|{path}"
                            if key not in seen:
                                seen.add(key)
                                routes.append({
                                    "method": method,
                                    "path": path,
                                    "handler": handler,
                                    "file": rel_path,
                                    "line": i + 1,
                                    "framework": "nestjs",
                                })
            except OSError:
                pass

        # ── Call-based routes (scan function calls) ──
        for call in file_data.get("function_calls", []):
            call_name = call.get("full_name", "") or call.get("name", "")

            # Skip false positive patterns (db.get, request.get, etc.)
            if any(call_name.startswith(prefix) for prefix in (
                "db.", "session.", "request.", "response.", "res.", "req.",
                "self.", "this.", "super.", "cls.", "console.", "logger.",
                "Math.", "JSON.", "Object.", "Array.", "String.",
                "os.", "sys.", "path.", "fs.",
            )):
                continue

            for pattern in _CALL_ROUTE_PATTERNS:
                m = pattern.search(call_name)
                if not m:
                    # Also check args for path string
                    args = call.get("args", [])
                    if args:
                        first_arg = str(args[0]).strip('"\'')
                        # Only match if first arg looks like a URL path
                        if first_arg.startswith("/") or first_arg.startswith("api/"):
                            combined = " ".join(str(a) for a in args[:2])
                            m = pattern.search(f'{call_name}({combined})')
                if m:
                    groups = m.groups()
                    if len(groups) == 2:
                        method = groups[0].upper()
                        path = groups[1]
                    else:
                        method = "ANY"
                        path = groups[0]

                    # Validate path looks like URL (not variable name)
                    if not path.startswith("/") and not path.startswith("api"):
                        continue

                    # Determine framework by language
                    if "HandleFunc" in call_name or lang == "go":
                        framework = "go"
                    elif "Route::" in call_name:
                        framework = "laravel"
                    elif "path(" in call_name:
                        framework = "django"
                    elif lang == "python":
                        framework = "fastapi"
                    elif lang in ("typescript", "javascript", "tsx"):
                        framework = "express"
                    elif lang == "java":
                        framework = "spring"
                    elif lang == "php":
                        framework = "laravel"
                    elif lang == "ruby":
                        framework = "rails"
                    else:
                        framework = "unknown"

                    handler = call.get("context", ("",))[0] if isinstance(call.get("context"), tuple) else ""
                    if not handler:
                        handler = call.get("name", "")

                    # For decorator-style calls (@router.get, @app.post) the parsed call
                    # name is an HTTP verb — not a real handler. Replace with the function
                    # declared immediately AFTER the decorator line (FastAPI/Flask pattern).
                    _HTTP_VERBS = {"get", "post", "put", "delete", "patch", "use", "all",
                                   "options", "head"}
                    if not handler or handler.lower() in _HTTP_VERBS:
                        call_line = call.get("line_number", 0) or 0
                        best_fn = None
                        best_dist = 999
                        for fn in file_data.get("functions", []):
                            fn_line = fn.get("line_number", 0) or 0
                            if fn_line > call_line and (fn_line - call_line) < best_dist:
                                best_dist = fn_line - call_line
                                best_fn = fn
                        if best_fn and best_dist <= 5:
                            handler = best_fn.get("name", "") or handler

                    key = f"{method}|{path}"
                    if key not in seen:
                        seen.add(key)
                        routes.append({
                            "method": method,
                            "path": path,
                            "handler": handler or "",
                            "file": rel_path,
                            "line": call.get("line_number", 0),
                            "framework": framework,
                        })
                    break

    # Sort by path
    routes.sort(key=lambda r: (r["path"], r["method"]))
    logger.info("Detected %d API routes", len(routes))
    return routes
