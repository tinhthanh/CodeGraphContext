"""Generate per-module context files for AI IDE wiki generation.

Incorporates patterns from:
- llm-wiki-v3 cgc_bridge.py (path normalization, structured queries)
- gen.py (dual path patterns, class categorization, architecture Mermaid)

Each module_contexts/{slug}.md is self-contained — AI IDE reads it
and generates one wiki document. Same data = same quality as server.
"""

from __future__ import annotations

import json
import logging
import os
from pathlib import Path
from typing import Any, Dict, List, Optional

logger = logging.getLogger(__name__)

_MAX_SOURCE_CHARS = 12000


def _rel(path: str, repo_path: str) -> str:
    """Normalize any path to repo-relative."""
    if not path:
        return path
    try:
        return os.path.relpath(path, repo_path)
    except ValueError:
        return Path(path).name


def _classify_class(rel_path: str) -> str:
    """Classify a class by its file path pattern."""
    p = rel_path.lower()
    if "controller" in p or "/api/" in p:
        return "controller"
    if "/service/" in p or "service.java" in p or "service.ts" in p:
        return "service"
    if "/model/" in p or "/domain/" in p or "/entity/" in p or "/entities/" in p:
        return "model"
    if "/repository/" in p or "repository.java" in p:
        return "repository"
    if "/event/" in p or "event.java" in p or "event.ts" in p:
        return "event"
    if "/dto/" in p or "dto.java" in p or "dto.ts" in p:
        return "dto"
    if "/guard/" in p or "/middleware/" in p:
        return "middleware"
    if "/config/" in p or "/configuration/" in p:
        return "config"
    if ".spec." in p or ".test." in p or "test" in p.split("/")[-1].lower():
        return "test"
    return "other"


def _build_arch_mermaid(layers: Dict[str, List[str]]) -> str:
    """Generate architecture Mermaid diagram from detected layers."""
    if not any(layers.values()):
        return ""

    lines = ["```mermaid", "graph LR"]

    if layers.get("controller"):
        lines.append("  Client[HTTP Client] --> Ctrl[Controllers]")
        if layers.get("service"):
            lines.append("  Ctrl --> Svc[Services]")
    elif layers.get("service"):
        lines.append("  Caller --> Svc[Services]")

    if layers.get("repository"):
        lines.append("  Svc --> Repo[Repositories]")
        lines.append("  Repo --> DB[(Database)]")

    if layers.get("event"):
        lines.append("  Svc --> Events[[Domain Events]]")

    if layers.get("middleware"):
        lines.append("  Middleware[Guards/Middleware] -.-> Ctrl")

    lines.append("```")
    return "\n".join(lines)


def generate_module_contexts(
    modules: List[Dict[str, Any]],
    parsed_results: List[Dict[str, Any]],
    repo_path: str,
    output_dir: str,
    call_groups: tuple = None,
    routes: List[Dict] = None,
    flows: List[Dict] = None,
    rationales: List[Dict] = None,
) -> int:
    """Generate module_contexts/{slug}.md for each module."""
    os.makedirs(output_dir, exist_ok=True)

    # Build lookup maps
    file_data_map: Dict[str, Dict] = {}  # rel_path → parsed data
    abs_to_rel: Dict[str, str] = {}      # absolute → relative

    for r in parsed_results:
        if "error" in r:
            continue
        fp = r.get("path", "")
        rel = _rel(fp, repo_path)
        file_data_map[rel] = r
        abs_to_rel[fp] = rel

    # Build call edges with NORMALIZED paths
    all_edges = []
    if call_groups:
        for group in call_groups:
            for e in group:
                caller_path = _rel(e.get("caller_file_path", ""), repo_path)
                called_path = _rel(e.get("called_file_path", ""), repo_path)
                same_file = caller_path == called_path
                all_edges.append({
                    "caller_name": e.get("caller_name", ""),
                    "caller_file": caller_path,
                    "called_name": e.get("called_name", ""),
                    "called_file": called_path,
                    "confidence": "EXTRACTED" if same_file else "INFERRED",
                })

    # Normalize route paths
    norm_routes = []
    if routes:
        for r in routes:
            nr = dict(r)
            # Routes already have relative paths from extract
            norm_routes.append(nr)

    # Normalize flow paths
    norm_flows = []
    if flows:
        for f in flows:
            nf = dict(f)
            nf["entry_file"] = _rel(f.get("entry_file", ""), repo_path)
            if "steps" in nf:
                for s in nf["steps"]:
                    if "file" in s:
                        s["file"] = _rel(s["file"], repo_path)
            norm_flows.append(nf)

    # Normalize rationale paths
    norm_rationales = []
    if rationales:
        for r in rationales:
            nr = dict(r)
            # Rationales already relative from extract
            norm_rationales.append(nr)

    count = 0

    for module in modules:
        slug = module["slug"]
        name = module["name"]
        files = module["files"]
        file_set = set(files)

        # Also match by path pattern (for DuckDB queries with absolute paths)
        # gen.py pattern: files might be under module dir
        module_dir = module.get("dir", slug)

        lines = []
        lines.append(f"# Module: {name}")
        lines.append("")

        # ── Stats ─────────────────────────────────────────────
        total_funcs = sum(len(file_data_map.get(f, {}).get("functions", [])) for f in files)
        total_classes = sum(len(file_data_map.get(f, {}).get("classes", [])) for f in files)
        module_routes = [r for r in norm_routes if r.get("file", "") in file_set
                         or any(r.get("file", "").startswith(f.rsplit("/", 1)[0]) for f in files[:3])]
        module_flows = [f for f in norm_flows if f.get("entry_file", "") in file_set
                        or any(f.get("entry_file", "").startswith(fp.rsplit("/", 1)[0]) for fp in files[:3])]
        module_rationales = [r for r in norm_rationales if r.get("file", "") in file_set]

        lines.append(f"**Stats:** {len(files)} files · {total_funcs} functions · {total_classes} classes · {len(module_routes)} routes")
        lines.append("")

        # ── Files ──────────────────────────────────────────────
        lines.append("## Files")
        lines.append("")
        for f in files[:40]:
            data = file_data_map.get(f, {})
            fn_count = len(data.get("functions", []))
            cls_count = len(data.get("classes", []))
            lines.append(f"- `{f}` ({fn_count} functions, {cls_count} classes)")
        if len(files) > 40:
            lines.append(f"- ... and {len(files) - 40} more")
        lines.append("")

        # ── Class categorization (gen.py pattern) ──────────────
        layers: Dict[str, List[str]] = {
            "controller": [], "service": [], "model": [],
            "repository": [], "event": [], "dto": [],
            "middleware": [], "config": [], "test": [], "other": [],
        }

        all_classes_in_module = []
        for f in files:
            data = file_data_map.get(f, {})
            for cls in data.get("classes", []):
                cls_name = cls.get("name", "")
                if not cls_name:
                    continue
                category = _classify_class(f)
                layers[category].append(cls_name)
                # Count methods
                methods = [fn.get("name", "") for fn in data.get("functions", [])
                           if fn.get("class_context") == cls_name]
                bases = cls.get("bases", [])
                bases_str = f" extends {', '.join(str(b) for b in bases)}" if bases else ""
                all_classes_in_module.append({
                    "name": cls_name, "file": f, "category": category,
                    "methods": methods, "bases_str": bases_str,
                    "method_count": len(methods),
                })

        # ── Architecture Mermaid ────────────────────────────────
        arch = _build_arch_mermaid(layers)
        if arch:
            lines.append("## Architecture")
            lines.append("")
            lines.append(arch)
            lines.append("")

        # ── Controllers ────────────────────────────────────────
        controllers = [c for c in all_classes_in_module if c["category"] == "controller"]
        if controllers:
            lines.append("## Controllers")
            lines.append("")
            for c in controllers:
                lines.append(f"- **`{c['name']}`**{c['bases_str']} — `{c['file']}` ({c['method_count']} methods)")
                if c["methods"][:8]:
                    lines.append(f"  Methods: {', '.join(c['methods'][:8])}")
            lines.append("")

        # ── Services ───────────────────────────────────────────
        services = [c for c in all_classes_in_module if c["category"] == "service"]
        if services:
            lines.append("## Services")
            lines.append("")
            for c in sorted(services, key=lambda x: -x["method_count"])[:20]:
                lines.append(f"- **`{c['name']}`**{c['bases_str']} — {c['method_count']} methods")
            lines.append("")

        # ── Models / DTOs ──────────────────────────────────────
        models = [c for c in all_classes_in_module if c["category"] in ("model", "dto")]
        if models:
            lines.append("## Models & DTOs")
            lines.append("")
            for c in models[:20]:
                lines.append(f"- `{c['name']}`{c['bases_str']} — `{c['file']}`")
            lines.append("")

        # ── API Routes ─────────────────────────────────────────
        if module_routes:
            lines.append("## API Routes / Endpoints")
            lines.append("")
            lines.append("| Method | Path | Handler | File |")
            lines.append("|--------|------|---------|------|")
            for r in module_routes:
                handler = r.get("handler", "-") or "-"
                lines.append(f"| {r['method']} | `{r['path']}` | `{handler}` | `{r.get('file', '')}` |")
            lines.append("")

        # ── Key Functions ──────────────────────────────────────
        lines.append("## Key Functions")
        lines.append("")
        sig_count = 0
        for f in files:
            data = file_data_map.get(f, {})
            for fn in data.get("functions", []):
                if sig_count >= 30:
                    break
                fn_name = fn.get("name", "")
                if not fn_name or fn_name.startswith("_"):
                    continue
                ctx = fn.get("class_context", "")
                args = fn.get("args", [])
                line_no = fn.get("line_number", 0)
                prefix = f"{ctx}." if ctx else ""
                args_str = ", ".join(str(a) for a in args[:5]) if args else ""
                lines.append(f"- `{prefix}{fn_name}({args_str})` — `{f}:{line_no}`")
                sig_count += 1
        lines.append("")

        # ── Call Graph ──────────────────────────────────────────
        intra = []
        outgoing = []
        incoming = []

        for e in all_edges:
            caller_in = e["caller_file"] in file_set
            called_in = e["called_file"] in file_set
            if caller_in and called_in:
                intra.append(e)
            elif caller_in:
                outgoing.append(e)
            elif called_in:
                incoming.append(e)

        lines.append("## Call Graph")
        lines.append("")

        if intra:
            lines.append("**Internal calls:**")
            seen = set()
            for e in intra[:25]:
                key = f"{e['caller_name']}→{e['called_name']}"
                if key not in seen:
                    seen.add(key)
                    lines.append(f"  {e['caller_name']} → {e['called_name']} [{e['confidence']}]")
            if len(intra) > 25:
                lines.append(f"  ... and {len(intra) - 25} more")
            lines.append("")

        if outgoing:
            lines.append("**Outgoing (this module calls):**")
            seen = set()
            for e in outgoing[:15]:
                key = f"{e['caller_name']}→{e['called_name']}"
                if key not in seen:
                    seen.add(key)
                    lines.append(f"  {e['caller_name']} → {e['called_name']} (`{e['called_file']}`) [INFERRED]")
            lines.append("")

        if incoming:
            lines.append("**Incoming (called by other modules):**")
            seen = set()
            for e in incoming[:15]:
                key = f"{e['caller_name']}→{e['called_name']}"
                if key not in seen:
                    seen.add(key)
                    lines.append(f"  {e['caller_name']} (`{e['caller_file']}`) → {e['called_name']} [INFERRED]")
            lines.append("")

        # ── Execution Flows ─────────────────────────────────────
        if module_flows:
            lines.append("## Execution Flows")
            lines.append("")
            for f in module_flows[:6]:
                steps = f.get("steps", [])
                lines.append(f"### `{f['name']}` ({f.get('step_count', len(steps))} steps, depth {f.get('depth', 0)})")
                lines.append(f"Entry: `{f.get('entry_file', '')}`")
                lines.append("")
                for i, s in enumerate(steps[:10], 1):
                    nm = s.get("name", str(s)) if isinstance(s, dict) else str(s)
                    lines.append(f"{i}. `{nm}`")
                if len(steps) > 10:
                    lines.append(f"... ({len(steps) - 10} more steps)")
                lines.append("")

        # ── Design Rationale ────────────────────────────────────
        if module_rationales:
            lines.append("## Design Rationale")
            lines.append("")
            for r in module_rationales:
                ctx = f" in `{r['context']}`" if r.get("context") else ""
                lines.append(f"- **[{r['tag']}]**{ctx}: {r['text']}")
                lines.append(f"  — `{r['file']}:{r['line']}`")
            lines.append("")

        # ── Source Code (key files only) ────────────────────────
        lines.append("## Source Code (key files)")
        lines.append("")
        chars = 0
        for f in files:
            if chars >= _MAX_SOURCE_CHARS:
                break
            data = file_data_map.get(f, {})
            fp = data.get("path", "")
            if not fp or not os.path.exists(fp):
                continue
            # Skip test files for source excerpts
            if _classify_class(f) == "test":
                continue
            try:
                content = open(fp, "r", encoding="utf-8", errors="replace").read()
            except OSError:
                continue
            if len(content) > 3000:
                content = content[:3000] + f"\n// ... ({len(content)} chars total)"
            ext = Path(f).suffix.lstrip(".")
            lines.append(f"### `{f}`")
            lines.append(f"```{ext}")
            lines.append(content)
            lines.append("```")
            lines.append("")
            chars += len(content)

        # ── Instructions ────────────────────────────────────────
        lines.extend([
            "", "---", "",
            "## Instructions for AI Wiki Generator", "",
            "Write comprehensive documentation for this module covering:",
            "1. **Purpose** — what this module does and why",
            "2. **Architecture** — key classes and how they relate (use/enhance the Mermaid diagram)",
            "3. **API Endpoints** — document each route with request/response if available",
            "4. **Execution Flows** — describe what happens when key functions are called",
            "5. **Design Decisions** — include rationale from source comments",
            "6. **Dependencies** — what this module depends on and what depends on it",
            "",
            "Use confidence tags: EXTRACTED = certain (AST), INFERRED = cross-file resolution.",
            "When mentioning another module, use: [[module-slug]]",
        ])

        # Write
        ctx_path = os.path.join(output_dir, f"{slug}.md")
        with open(ctx_path, "w") as fh:
            fh.write("\n".join(lines))
        count += 1

    # ── index.md ────────────────────────────────────────────────
    idx_lines = [
        "# Wiki Generation Index", "",
        f"**{len(modules)} modules** ready for wiki generation.", "",
        "| Module | Files | Functions | Classes | Routes | Slug |",
        "|--------|-------|-----------|---------|--------|------|",
    ]
    for m in modules:
        fs = set(m["files"])
        fn_c = sum(len(file_data_map.get(f, {}).get("functions", [])) for f in m["files"])
        cl_c = sum(len(file_data_map.get(f, {}).get("classes", [])) for f in m["files"])
        rt_c = sum(1 for r in (norm_routes or []) if r.get("file", "") in fs)
        idx_lines.append(f"| {m['name']} | {m['file_count']} | {fn_c} | {cl_c} | {rt_c} | `{m['slug']}` |")

    idx_lines.extend([
        "", "## Generate Wiki", "",
        "For each module: read `module_contexts/{slug}.md` → generate `wiki-output/{slug}.md`",
        "After all modules: generate `wiki-output/overview.md`",
    ])

    with open(os.path.join(output_dir, "index.md"), "w") as fh:
        fh.write("\n".join(idx_lines))
    count += 1

    # Save modules.json
    with open(os.path.join(os.path.dirname(output_dir), "modules.json"), "w") as fh:
        json.dump(modules, fh, indent=2)

    logger.info("Generated %d module context files", count)
    return count
