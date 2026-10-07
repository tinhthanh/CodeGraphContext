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

# Function call-based routes (Express, Hono, Go Fiber/Gin/Echo/Chi, ...)
# Generic pattern: <any identifier>.<Verb>("<path starting with />", ...)
# Works for: app.get / router.Post / translationGroup.Get / r.GET / api.use / etc.
# Case-insensitive to cover JS (get) + Go (Get/GET) + mixed conventions.
# Require path arg to start with "/" — reduces false positives like res.get("header").
_CALL_ROUTE_PATTERNS = [
    re.compile(
        r'\b[\w.]+\.(get|post|put|delete|patch|head|options|use|all)\s*\(\s*["\'](\/[^"\']*)["\']',
        re.IGNORECASE,
    ),
    # Go net/http
    re.compile(r'(?:http\.)?HandleFunc\s*\(\s*["\'](\/[^"\']*)["\']'),
    # Laravel
    re.compile(r'Route::(get|post|put|delete|patch)\s*\(\s*["\'](\/[^"\']*)["\']', re.IGNORECASE),
    # Django
    re.compile(r'\bpath\s*\(\s*["\']([^"\']+)["\']'),
]

# Next.js / Nuxt file-based routing patterns
_NEXTJS_ROUTE_FILES = {"page.tsx", "page.ts", "page.jsx", "page.js", "route.tsx", "route.ts"}
_NEXTJS_ROUTE_GROUPS = re.compile(r'\(([^)]+)\)')  # strip (group) from path


# ── Spring (Java/Kotlin) annotation scanning ───────────────────────

_SPRING_MAPPING_RE = re.compile(r'@(Get|Post|Put|Delete|Patch|Request)Mapping\b')
_STRING_LITERAL_RE = re.compile(r'"((?:[^"\\]|\\.)*)"')
_NAMED_PATH_ARG_RE = re.compile(r'\b(?:value|path)\s*=\s*')
_REQUEST_METHOD_RE = re.compile(r'RequestMethod\.(GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS)\b')
_TYPE_DECL_RE = re.compile(r'\b(?:class|interface|object)\s+[A-Z]\w*')


def _strip_jvm_comments(src: str) -> str:
    """Blank out // and /* */ comments, keeping string literals and newlines.

    Line numbers are preserved so offsets still map to source lines. String
    literals are honoured, so `"/api/**"` or `// see /api/*` cannot open a
    phantom block comment.
    """
    out = []
    i, n = 0, len(src)
    while i < n:
        ch = src[i]
        two = src[i:i + 2]
        if src.startswith('"""', i):  # Java text block / Kotlin raw string
            j = src.find('"""', i + 3)
            j = n if j == -1 else j + 3
            out.append(src[i:j]); i = j
        elif ch == '"' or ch == "'":
            j = i + 1
            while j < n and src[j] != ch and src[j] != "\n":
                j += 2 if src[j] == "\\" else 1
            j = min(j + 1, n)
            out.append(src[i:j]); i = j
        elif two == "//":
            j = src.find("\n", i)
            j = n if j == -1 else j
            out.append(" " * (j - i)); i = j
        elif two == "/*":
            j = src.find("*/", i + 2)
            j = n if j == -1 else j + 2
            out.append(re.sub(r"[^\n]", " ", src[i:j])); i = j
        else:
            out.append(ch); i += 1
    return "".join(out)


def _balanced_args(code: str, pos: int) -> Optional[str]:
    """Return the text inside the parenthesis group starting at/after ``pos``.

    None when the annotation has no argument list (bare ``@GetMapping``).
    """
    j = pos
    while j < len(code) and code[j] in " \t\r\n":
        j += 1
    if j >= len(code) or code[j] != "(":
        return None
    depth, k = 0, j
    while k < len(code):
        c = code[k]
        if c == '"':
            k += 1
            while k < len(code) and code[k] != '"':
                k += 2 if code[k] == "\\" else 1
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0:
                return code[j + 1:k]
        k += 1
    return code[j + 1:]


def _leading_value(args: str) -> str:
    """The first argument expression (a string, or an array of strings)."""
    a = args.lstrip()
    for opener, closer in (("{", "}"), ("[", "]"), ("arrayOf(", ")")):
        if a.startswith(opener):
            end = a.find(closer, len(opener))
            return a[: end + 1] if end != -1 else a
    m = _STRING_LITERAL_RE.match(a)
    return m.group(0) if m else ""


def _mapping_paths(args: Optional[str]) -> List[str]:
    """Paths declared by a mapping annotation; [""] when it declares none."""
    if args is None:
        return [""]
    value = _leading_value(args)
    if not value:
        m = _NAMED_PATH_ARG_RE.search(args)
        if m:
            value = _leading_value(args[m.end():])
    paths = [p for p in _STRING_LITERAL_RE.findall(value)]
    return paths or [""]


def _join_route(prefix: str, path: str) -> str:
    prefix = prefix.rstrip("/")
    if path and not path.startswith("/"):
        path = "/" + path
    joined = prefix + path
    return joined if joined.startswith("/") else "/" + joined


def _extract_spring_mappings(code: str) -> List[tuple]:
    """Return ``(METHOD, path, line)`` for each Spring handler mapping.

    Handles bare annotations (``@GetMapping``), ``value=``/``path=``, array
    forms (``@GetMapping({"", "/search"})``), multi-line argument lists and
    method-level ``@RequestMapping(method = RequestMethod.X)``. A
    ``@RequestMapping`` placed before the first type declaration is treated
    as the class-level prefix.
    """
    first_type = _TYPE_DECL_RE.search(code)
    type_pos = first_type.start() if first_type else -1

    prefixes = [""]
    mappings = []
    for m in _SPRING_MAPPING_RE.finditer(code):
        kind = m.group(1)
        args = _balanced_args(code, m.end())
        if kind == "Request" and 0 <= m.start() < type_pos:
            prefixes = _mapping_paths(args)
            continue
        if kind == "Request":
            methods = _REQUEST_METHOD_RE.findall(args or "") or ["ANY"]
        else:
            methods = [kind.upper()]
        line = code.count("\n", 0, m.start()) + 1
        mappings.append((methods, _mapping_paths(args), line))

    results = []
    for methods, paths, line in mappings:
        for prefix in prefixes:
            for path in paths:
                for method in methods:
                    results.append((method, _join_route(prefix, path), line))
    return results


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
                is_route_file = file_name.startswith("route.")

                if is_route_file and os.path.exists(file_path):
                    # App Router `route.ts` exports one function per HTTP method:
                    #   export async function GET(req) { ... }
                    #   export async function POST(req) { ... }
                    # Emit one route per exported method.
                    try:
                        with open(file_path, "r", encoding="utf-8", errors="replace") as fh:
                            src = fh.read()
                    except OSError:
                        src = ""
                    method_re = re.compile(
                        r'export\s+(?:async\s+)?function\s+(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\b',
                    )
                    found_methods = set()
                    for m in method_re.finditer(src):
                        method = m.group(1).upper()
                        if method in found_methods:
                            continue
                        found_methods.add(method)
                        key = f"{method}|{route}"
                        if key in seen:
                            continue
                        seen.add(key)
                        routes.append({
                            "method": method,
                            "path": route,
                            "handler": method,  # route.ts handler IS the method
                            "file": rel_path,
                            "line": 0,
                            "framework": "nextjs",
                        })
                    if not found_methods:
                        # Fallback: emit a GET route with file name
                        key = f"GET|{route}"
                        if key not in seen:
                            seen.add(key)
                            routes.append({
                                "method": "GET", "path": route,
                                "handler": "", "file": rel_path,
                                "line": 0, "framework": "nextjs",
                            })
                else:
                    # page.tsx: single GET route. Handler = `export default function ...`
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
        # Java/Kotlin annotations also land in `decorators`, but Spring routes
        # need the class-level @RequestMapping prefix: the source scan below
        # handles them, so skip them here to avoid duplicate/unprefixed routes.
        decorated_fns = [] if lang in ("java", "kotlin") else file_data.get("functions", [])
        for fn in decorated_fns:
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
        # so scan the (comment-stripped) source for Spring mappings.
        if lang in ("java", "kotlin") and os.path.exists(file_path):
            try:
                with open(file_path, "r", encoding="utf-8", errors="replace") as fh:
                    code = _strip_jvm_comments(fh.read())
            except OSError:
                code = ""
            functions = file_data.get("functions", [])
            for method, path, line in _extract_spring_mappings(code):
                # Handler: closest function within a few lines of the annotation.
                # Rust parser sometimes reports function line as FIRST annotation
                # (e.g. @Scheduled above @GetMapping), so we look both directions.
                handler = ""
                best_dist = 999
                for fn in functions:
                    dist = abs(fn.get("line_number", 0) - line)
                    if dist < best_dist:
                        best_dist = dist
                        handler = fn.get("name", "")
                if best_dist > 8:
                    handler = ""

                # Keyed per file: in a multi-service repo a gateway and a
                # downstream service legitimately expose the same path.
                key = f"{method}|{path}|{rel_path}"
                if key not in seen:
                    seen.add(key)
                    routes.append({
                        "method": method,
                        "path": path,
                        "handler": handler,
                        "file": rel_path,
                        "line": line,
                        "framework": "spring",
                    })

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
                        stripped = line.lstrip()
                        if stripped.startswith("//") or stripped.startswith("*"):
                            continue
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

        # ── Go source scan (Fiber, Gin, Echo, Chi) ─────────────
        # Rust parser doesn't populate function_call args for Go, so scan source.
        if lang == "go" and os.path.exists(file_path):
            try:
                with open(file_path, "r", encoding="utf-8", errors="replace") as fh:
                    src_lines = fh.readlines()
                go_route_re = re.compile(
                    r'\b(\w+)\.(Get|Post|Put|Delete|Patch|Head|Options|Use|All|GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS|USE|ALL)'
                    r'\s*\(\s*["\'](\/[^"\']*)["\']\s*,\s*([\w.]+)'
                )
                # Skip common non-route method calls
                _SKIP_GO_RECV = {"ctx", "c", "req", "request", "res", "response",
                                 "header", "headers", "url", "w", "writer"}
                for i, line in enumerate(src_lines):
                    stripped = line.lstrip()
                    if stripped.startswith("//") or stripped.startswith("*"):
                        continue
                    m = go_route_re.search(line)
                    if not m:
                        continue
                    recv, verb, path, handler = m.groups()
                    if recv.lower() in _SKIP_GO_RECV:
                        continue
                    method = verb.upper()
                    if method in ("USE", "ALL"):
                        method = "USE"
                    key = f"{method}|{path}"
                    if key in seen:
                        continue
                    seen.add(key)
                    routes.append({
                        "method": method,
                        "path": path,
                        "handler": handler,
                        "file": rel_path,
                        "line": i + 1,
                        "framework": "go",
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
                    # Exception: Express/Hono/Fiber inline arrow handlers — the handler is
                    # an anonymous arrow on the same line, NOT a named function below.
                    _HTTP_VERBS = {"get", "post", "put", "delete", "patch", "use", "all",
                                   "options", "head"}
                    if not handler or handler.lower() in _HTTP_VERBS:
                        call_line = call.get("line_number", 0) or 0
                        # Detect inline arrow/function on the call line itself
                        is_inline = False
                        if call_line > 0 and os.path.exists(file_path) and lang in (
                            "javascript", "typescript", "tsx"
                        ):
                            try:
                                with open(file_path, "r", encoding="utf-8", errors="replace") as fh:
                                    src_lines = fh.readlines()
                                # Look at call_line through call_line+3 (multi-line calls)
                                window = "".join(src_lines[call_line - 1: call_line + 3])
                                if re.search(r'=>\s*[{(]|\bfunction\s*\(', window):
                                    is_inline = True
                            except OSError:
                                pass
                        if is_inline:
                            handler = "<anonymous>"
                        else:
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
