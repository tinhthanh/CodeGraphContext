"""Detect execution flows (entry points → call chain traces).

An execution flow starts at an "entry point" (API handler, main function,
test function, CLI command, scheduled task) and traces through the call
graph via BFS to show the full execution path.

This gives AI agents/LLMs a "what happens when X is called" view.
"""

from __future__ import annotations

import logging
from collections import deque
from typing import Any, Dict, List, Optional, Set, Tuple

logger = logging.getLogger(__name__)

# ── Entry point detection patterns ──────────────────────────────────

# Function name patterns that indicate entry points
ENTRY_POINT_PATTERNS = {
    # Main / CLI
    "main": 10,
    "run": 3,
    "start": 3,
    "execute": 3,
    "cli": 3,
    # API handlers (Express, FastAPI, Spring, etc.)
    "handler": 5,
    "handle": 4,
    "controller": 5,
    # HTTP methods
    "get": 2,
    "post": 2,
    "put": 2,
    "delete": 2,
    "patch": 2,
    # Test functions
    "test": 4,
    "it": 3,
    "describe": 3,
    "spec": 3,
    # Lifecycle hooks
    "ngOnInit": 5,
    "componentDidMount": 5,
    "useEffect": 2,
    "onCreate": 5,
    "onStart": 5,
    # Scheduled tasks
    "schedule": 4,
    "cron": 4,
    "job": 3,
    "task": 3,
    "worker": 3,
}

# Decorator patterns that indicate entry points
ENTRY_DECORATORS = {
    "@app.route",
    "@app.get",
    "@app.post",
    "@app.put",
    "@app.delete",
    "@router.get",
    "@router.post",
    "@api_view",
    "@action",
    "@GetMapping",
    "@PostMapping",
    "@PutMapping",
    "@DeleteMapping",
    "@RequestMapping",
    "@Controller",
    "@RestController",
    "@Service",
    "@Scheduled",
    "@Test",
    "@test",
    "@pytest.fixture",
    "@click.command",
    "@celery.task",
}

# Class patterns that indicate entry point containers
ENTRY_CLASS_PATTERNS = {
    "Controller",
    "Handler",
    "Resolver",
    "Resource",
    "Endpoint",
    "View",
    "Servlet",
    "Component",
    "Page",
    "Service",
}


def score_entry_point(func: Dict[str, Any]) -> int:
    """Score how likely a function is an entry point (0 = not, higher = more likely)."""
    score = 0
    name = func.get("name", "").lower()
    decorators = func.get("decorators", []) or []
    class_ctx = func.get("class_context", "") or ""

    # Name-based scoring
    for pattern, points in ENTRY_POINT_PATTERNS.items():
        if pattern in name:
            score += points
            break

    # Decorator-based scoring
    for dec in decorators:
        dec_str = str(dec).strip()
        for entry_dec in ENTRY_DECORATORS:
            if entry_dec.lower() in dec_str.lower():
                score += 8
                break

    # Class context scoring
    for cls_pattern in ENTRY_CLASS_PATTERNS:
        if cls_pattern.lower() in class_ctx.lower():
            score += 3
            break

    # Boost for exported/public functions (no leading underscore)
    if name and not name.startswith("_"):
        score += 1

    # Boost for functions with no callers (leaf nodes in reverse graph)
    # This is checked during flow detection, not here

    return score


def detect_execution_flows(
    parsed_results: List[Dict[str, Any]],
    call_groups: Tuple,
    max_flows: int = 300,
    max_depth: int = 10,
) -> List[Dict[str, Any]]:
    """Detect execution flows from entry points through call graph.

    Args:
        parsed_results: List of parsed file data (from Rust parser)
        call_groups: 6-tuple of call edge lists (fn_fn, fn_cls, ...)
        max_flows: Maximum number of flows to detect
        max_depth: Maximum BFS depth per flow

    Returns:
        List of flow dicts: {
            name: str,           # entry point name
            entry_file: str,     # file path
            entry_line: int,     # line number
            entry_class: str,    # class context (if any)
            steps: [{name, file, line, depth}],  # ordered call chain
            depth: int,          # max depth reached
            score: int,          # entry point score
        }
    """
    # Build call graph adjacency list: caller_key → [(callee_name, callee_path)]
    adj: Dict[str, List[Tuple[str, str, int]]] = {}  # key → [(name, path, line)]

    for group in call_groups:
        for edge in group:
            caller_name = edge.get("caller_name", "")
            caller_path = edge.get("caller_file_path", "")
            called_name = edge.get("called_name", "")
            called_path = edge.get("called_file_path", "")
            line = edge.get("line_number", 0)

            if not caller_name or not called_name:
                continue

            key = f"{caller_name}|{caller_path}"
            if key not in adj:
                adj[key] = []
            adj[key].append((called_name, called_path, line))

    # Build reverse graph to find functions with no callers
    has_callers: Set[str] = set()
    for group in call_groups:
        for edge in group:
            called_name = edge.get("called_name", "")
            called_path = edge.get("called_file_path", "")
            if called_name:
                has_callers.add(f"{called_name}|{called_path}")

    # Score all functions as potential entry points
    candidates: List[Tuple[int, Dict[str, Any], str]] = []

    for file_data in parsed_results:
        if "error" in file_data:
            continue
        file_path = file_data.get("path", "")

        for func in file_data.get("functions", []):
            func_name = func.get("name", "")
            func_key = f"{func_name}|{file_path}"

            score = score_entry_point(func)

            # Boost for functions with no callers (true entry points)
            if func_key not in has_callers:
                score += 2

            if score >= 3:  # minimum threshold
                candidates.append((score, {**func, "path": file_path}, func_key))

    # Sort by score descending, take top candidates
    candidates.sort(key=lambda x: -x[0])
    candidates = candidates[:max_flows * 2]  # over-fetch, will filter

    # Noise function names to skip during BFS (not interesting for flow tracing)
    noise_names: Set[str] = {
        # Java/generic
        "ok", "of", "build", "builder", "toString", "hashCode", "equals",
        "get", "set", "put", "add", "remove", "contains", "size", "isEmpty",
        "valueOf", "values", "name", "ordinal", "compareTo",
        "matcher", "matches", "group", "find", "pattern",
        "append", "format", "trim", "split", "join", "replace",
        "parseInt", "parseFloat", "parseLong", "parseDouble",
        "asList", "singletonList", "emptyList", "emptyMap",
        "HashMap<>", "ArrayList<>", "HashSet<>", "LinkedList<>",
        "Optional", "ofNullable", "orElse", "orElseThrow", "isPresent",
        "stream", "map", "filter", "collect", "forEach", "reduce", "flatMap",
        "toList", "toSet", "toMap", "joining",
        # Jackson / JSON
        "asText", "asLong", "asInt", "asBoolean", "isMissingNode", "isArray",
        "path", "readTree", "readValue", "writeValueAsString",
        "jsonPath", "contentType", "content",
        # Java time / lang
        "currentTimeMillis", "nanoTime", "now", "toInstant", "atZone",
        "getTime", "toLocalDate", "format",
        # Java accessors (common getters)
        "getId", "getName", "getStatus", "getType", "getValue",
        "getCode", "getMessage", "getPath", "getClass",
        "setId", "setName", "setStatus", "setType",
        # BigDecimal / AtomicReference
        "BigDecimal", "AtomicReference", "AtomicInteger", "AtomicLong",
        "ZERO", "ONE", "TEN", "compareTo", "multiply", "divide",
        # JS/TS generic
        "then", "catch", "finally", "resolve", "reject",
        "push", "pop", "shift", "slice", "splice", "concat",
        "keys", "entries", "assign", "freeze", "create",
        "log", "warn", "error", "info", "debug",
        "JSON", "stringify", "parse",
        "setTimeout", "setInterval", "clearTimeout", "clearInterval",
        "Promise", "async", "await",
        "require", "module", "exports",
        "console", "document", "window",
        # React
        "useState", "useEffect", "useRef", "useCallback", "useMemo",
        "useContext", "useReducer",
        "preventDefault", "stopPropagation",
        # Python
        "print", "len", "range", "str", "int", "float", "bool", "list", "dict",
        "super", "self", "cls",
    }

    # BFS from each entry point
    flows: List[Dict[str, Any]] = []
    seen_flows: Set[str] = set()  # avoid duplicate flows

    for score, func, func_key in candidates:
        if len(flows) >= max_flows:
            break

        # BFS
        steps: List[Dict[str, Any]] = []
        visited: Set[str] = set()
        queue: deque = deque()

        # Start from entry point
        entry_name = func.get("name", "")
        entry_path = func.get("path", "")
        entry_line = func.get("line_number", 0)
        entry_class = func.get("class_context", "") or ""

        queue.append((entry_name, entry_path, 0))
        visited.add(func_key)

        while queue and len(steps) < 50:  # max 50 steps per flow
            name, path, depth = queue.popleft()

            if depth > max_depth:
                continue

            # Skip noise names from steps (keep entry point even if noisy)
            if depth > 0 and name in noise_names:
                continue

            steps.append({
                "name": name,
                "file": path,
                "line": 0,
                "depth": depth,
            })

            # Follow outgoing calls (skip noise)
            key = f"{name}|{path}"
            for callee_name, callee_path, callee_line in adj.get(key, []):
                if callee_name in noise_names:
                    continue
                callee_key = f"{callee_name}|{callee_path}"
                if callee_key not in visited:
                    visited.add(callee_key)
                    queue.append((callee_name, callee_path, depth + 1))

        # Skip trivial flows (< 3 meaningful steps)
        if len(steps) < 3:
            continue

        # Deduplicate by first 3 steps
        flow_sig = "|".join(s["name"] for s in steps[:3])
        if flow_sig in seen_flows:
            continue
        seen_flows.add(flow_sig)

        flow_name = entry_name
        if entry_class:
            flow_name = f"{entry_class}.{entry_name}"

        flows.append({
            "name": flow_name,
            "entry_file": entry_path,
            "entry_line": entry_line,
            "entry_class": entry_class,
            "steps": steps,
            "depth": max(s["depth"] for s in steps),
            "step_count": len(steps),
            "score": score,
        })

    flows.sort(key=lambda x: (-x["score"], -x["step_count"]))
    logger.info("Detected %d execution flows from %d candidates", len(flows), len(candidates))
    return flows[:max_flows]
