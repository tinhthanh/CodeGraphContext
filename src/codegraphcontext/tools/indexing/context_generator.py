"""Generate per-module context files for AI IDE wiki generation.

Each module_contexts/{slug}.md contains exactly the same data that
the server LLM prompt (MODULE_USER_CGC) provides — routes, flows,
rationale, signatures, call graph, source code excerpts.

AI IDE reads these files + SKILL.md instructions → generates wiki docs.
Same input data = same output quality. $0 cost.
"""

from __future__ import annotations

import json
import logging
import os
from pathlib import Path
from typing import Any, Dict, List

logger = logging.getLogger(__name__)

# Max source code chars per module context
_MAX_SOURCE_CHARS = 12000


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
    """Generate module_contexts/{slug}.md for each module.

    Returns number of context files written.
    """
    os.makedirs(output_dir, exist_ok=True)

    # Build lookup maps
    file_data_map: Dict[str, Dict] = {}
    for r in parsed_results:
        if "error" in r:
            continue
        fp = r.get("path", "")
        try:
            rel = os.path.relpath(fp, repo_path)
        except ValueError:
            rel = Path(fp).name
        file_data_map[rel] = r

    # Build call edge lookup
    all_edges = []
    if call_groups:
        for group in call_groups:
            for e in group:
                caller_path = e.get("caller_file_path", "")
                called_path = e.get("called_file_path", "")
                try:
                    caller_rel = os.path.relpath(caller_path, repo_path)
                    called_rel = os.path.relpath(called_path, repo_path)
                except ValueError:
                    caller_rel = Path(caller_path).name
                    called_rel = Path(called_path).name
                all_edges.append({
                    "caller_name": e.get("caller_name", ""),
                    "caller_file": caller_rel,
                    "called_name": e.get("called_name", ""),
                    "called_file": called_rel,
                    "confidence": "EXTRACTED" if caller_path == called_path else "INFERRED",
                })

    count = 0
    all_module_files = set()
    for m in modules:
        all_module_files.update(m["files"])

    for module in modules:
        slug = module["slug"]
        name = module["name"]
        files = module["files"]
        file_set = set(files)

        lines = []
        lines.append(f"# Module: {name}")
        lines.append("")
        lines.append(f"## Files in this module ({len(files)})")
        lines.append("")
        for f in files[:50]:
            data = file_data_map.get(f, {})
            fn_count = len(data.get("functions", []))
            cls_count = len(data.get("classes", []))
            lines.append(f"- `{f}` ({fn_count} functions, {cls_count} classes)")
        if len(files) > 50:
            lines.append(f"- ... and {len(files) - 50} more files")

        # ── Function Signatures ────────────────────────────────
        lines.append("")
        lines.append("## Function Signatures")
        lines.append("")
        sig_count = 0
        for f in files:
            data = file_data_map.get(f, {})
            for fn in data.get("functions", []):
                if sig_count >= 40:
                    break
                fn_name = fn.get("name", "")
                args = fn.get("args", [])
                line_no = fn.get("line_number", 0)
                ctx = fn.get("class_context", "")
                decorators = fn.get("decorators", [])
                docstring = (fn.get("docstring", "") or "")[:100]

                prefix = f"{ctx}." if ctx else ""
                args_str = ", ".join(str(a) for a in args[:5]) if args else ""
                dec_str = f" [{', '.join(str(d) for d in decorators)}]" if decorators else ""

                sig = f"- `{prefix}{fn_name}({args_str})`{dec_str} — `{f}:{line_no}`"
                if docstring:
                    sig += f"\n  {docstring}"
                lines.append(sig)
                sig_count += 1

        # ── Classes ────────────────────────────────────────────
        classes = module.get("classes", [])
        if classes:
            lines.append("")
            lines.append("## Classes")
            lines.append("")
            for cls_name in classes[:20]:
                # Find methods
                methods = []
                for f in files:
                    data = file_data_map.get(f, {})
                    for fn in data.get("functions", []):
                        if fn.get("class_context") == cls_name:
                            methods.append(fn.get("name", ""))
                methods_str = ", ".join(methods[:8])
                if len(methods) > 8:
                    methods_str += f" ... ({len(methods)} total)"
                lines.append(f"- **{cls_name}**: {methods_str}")

        # ── API Routes ─────────────────────────────────────────
        module_routes = []
        if routes:
            for r in routes:
                r_file = r.get("file", "")
                if r_file in file_set or any(r_file.endswith(f) for f in files):
                    module_routes.append(r)

        if module_routes:
            lines.append("")
            lines.append("## API Routes / Endpoints")
            lines.append("")
            lines.append("| Method | Path | Handler | Framework |")
            lines.append("|--------|------|---------|-----------|")
            for r in module_routes:
                lines.append(f"| {r['method']} | `{r['path']}` | {r['handler']} | {r['framework']} |")

        # ── Call Graph ──────────────────────────────────────────
        intra = []
        outgoing = []
        incoming = []

        for e in all_edges:
            caller_in = e["caller_file"] in file_set
            called_in = e["called_file"] in file_set
            if caller_in and called_in:
                intra.append(e)
            elif caller_in and not called_in:
                outgoing.append(e)
            elif not caller_in and called_in:
                incoming.append(e)

        lines.append("")
        lines.append("## Call Graph")
        lines.append("")

        if intra:
            lines.append("**Internal calls (within this module):**")
            seen = set()
            for e in intra[:30]:
                key = f"{e['caller_name']}→{e['called_name']}"
                if key not in seen:
                    seen.add(key)
                    conf = f" [{e['confidence']}]"
                    lines.append(f"  {e['caller_name']} → {e['called_name']}{conf}")
            lines.append("")

        if outgoing:
            lines.append("**Outgoing calls (this module calls):**")
            seen = set()
            for e in outgoing[:20]:
                key = f"{e['caller_name']}→{e['called_name']}"
                if key not in seen:
                    seen.add(key)
                    lines.append(f"  {e['caller_name']} → {e['called_name']} ({e['called_file']}) [INFERRED]")
            lines.append("")

        if incoming:
            lines.append("**Incoming calls (called by other modules):**")
            seen = set()
            for e in incoming[:20]:
                key = f"{e['caller_name']}→{e['called_name']}"
                if key not in seen:
                    seen.add(key)
                    lines.append(f"  {e['caller_name']} ({e['caller_file']}) → {e['called_name']} [INFERRED]")
            lines.append("")

        # ── Execution Flows ─────────────────────────────────────
        module_flows = []
        if flows:
            for f in flows:
                entry_file = f.get("entry_file", "")
                if entry_file in file_set or any(entry_file.endswith(fp) for fp in files):
                    module_flows.append(f)

        if module_flows:
            lines.append("")
            lines.append("## Execution Flows")
            lines.append("")
            for f in module_flows[:8]:
                steps = f.get("steps", [])
                chain = " → ".join(s["name"] for s in steps[:6])
                if len(steps) > 6:
                    chain += f" → ... ({f['step_count']} steps)"
                lines.append(f"**{f['name']}** (`{f.get('entry_file', '')}:{f.get('entry_line', 0)}`)")
                lines.append(f"  {chain}")
                lines.append("")

        # ── Design Rationale ────────────────────────────────────
        module_rationale = []
        if rationales:
            for r in rationales:
                if r.get("file", "") in file_set:
                    module_rationale.append(r)

        if module_rationale:
            lines.append("")
            lines.append("## Design Rationale (from source comments)")
            lines.append("")
            for r in module_rationale:
                ctx = f" in `{r['context']}`" if r.get("context") else ""
                lines.append(f"- **[{r['tag']}]**{ctx}: {r['text']}")
                lines.append(f"  — `{r['file']}:{r['line']}`")

        # ── Source Code Excerpts ────────────────────────────────
        lines.append("")
        lines.append("## Source Code (key files)")
        lines.append("")

        chars_used = 0
        for f in files:
            if chars_used >= _MAX_SOURCE_CHARS:
                lines.append(f"... (source truncated, {len(files) - files.index(f)} files remaining)")
                break

            data = file_data_map.get(f, {})
            file_path = data.get("path", "")
            if not file_path or not os.path.exists(file_path):
                continue

            try:
                with open(file_path, "r", encoding="utf-8", errors="replace") as fh:
                    content = fh.read()
            except OSError:
                continue

            # Truncate large files
            if len(content) > 3000:
                content = content[:3000] + f"\n... ({len(content)} chars total, truncated)"

            ext = Path(f).suffix.lstrip(".")
            lines.append(f"### `{f}`")
            lines.append(f"```{ext}")
            lines.append(content)
            lines.append("```")
            lines.append("")
            chars_used += len(content)

        # ── Instructions for AI ─────────────────────────────────
        lines.append("")
        lines.append("---")
        lines.append("")
        lines.append("## Instructions for AI Wiki Generator")
        lines.append("")
        lines.append("Write comprehensive documentation for this module. Cover:")
        lines.append("1. **Purpose** — what this module does and why it exists")
        lines.append("2. **Architecture** — key classes/functions and how they relate (include Mermaid diagram)")
        lines.append("3. **API Endpoints** — if routes exist, document each endpoint")
        lines.append("4. **Execution Flows** — describe what happens when key functions are called")
        lines.append("5. **Design Decisions** — include rationale from source comments")
        lines.append("6. **Dependencies** — what this module depends on and what depends on it")
        lines.append("")
        lines.append("Use the call graph confidence tags: EXTRACTED = certain (same file), INFERRED = cross-file resolution.")
        lines.append("When mentioning another module, use wikilink syntax: [[module-slug]]")

        # Write file
        context_path = os.path.join(output_dir, f"{slug}.md")
        with open(context_path, "w") as fh:
            fh.write("\n".join(lines))
        count += 1

    # ── Generate index.md ───────────────────────────────────────
    index_lines = [
        "# Wiki Generation Index",
        "",
        f"This codebase has been indexed into **{len(modules)} modules**.",
        "Each module has a context file in this directory with all the data",
        "needed to generate comprehensive documentation.",
        "",
        "## Modules",
        "",
        "| Module | Files | Classes | Routes | Slug |",
        "|--------|-------|---------|--------|------|",
    ]
    for m in modules:
        r_count = sum(1 for r in (routes or []) if r.get("file", "") in set(m["files"]))
        index_lines.append(
            f"| {m['name']} | {m['file_count']} | {len(m['classes'])} | {r_count} | `{m['slug']}` |"
        )

    index_lines.extend([
        "",
        "## How to Generate Wiki",
        "",
        "For each module above:",
        "1. Read `module_contexts/{slug}.md`",
        "2. Generate a wiki document following the instructions at the bottom",
        "3. Save to `wiki-output/{slug}.md`",
        "4. After all modules, generate `wiki-output/overview.md`",
    ])

    with open(os.path.join(output_dir, "index.md"), "w") as fh:
        fh.write("\n".join(index_lines))
    count += 1

    # ── Save modules.json ───────────────────────────────────────
    modules_json = os.path.join(os.path.dirname(output_dir), "modules.json")
    with open(modules_json, "w") as fh:
        json.dump(modules, fh, indent=2)

    logger.info("Generated %d module context files in %s", count, output_dir)
    return count
