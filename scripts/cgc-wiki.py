#!/usr/bin/env python3
"""cgc-wiki: Index codebase → generate wiki-ready graph data.

Usage:
    python cgc-wiki.py index /path/to/repo
    python cgc-wiki.py index /path/to/repo --output ./cgc-index
    python cgc-wiki.py report /path/to/repo
    python cgc-wiki.py query /path/to/repo "search term"
"""

import argparse
import json
import os
import sys
import time
from pathlib import Path

# Add CGC to path
CGC_SRC = str(Path(__file__).parent.parent / "src")
if CGC_SRC not in sys.path:
    sys.path.insert(0, CGC_SRC)


EXT_LANG = {
    ".py": "python", ".js": "javascript", ".jsx": "javascript",
    ".ts": "typescript", ".tsx": "tsx",
    ".java": "java", ".go": "go", ".rs": "rust",
    ".rb": "ruby", ".cs": "c_sharp", ".php": "php",
    ".kt": "kotlin", ".scala": "scala", ".swift": "swift",
    ".c": "c", ".cpp": "cpp", ".h": "c", ".hpp": "cpp",
    ".dart": "dart", ".ex": "elixir", ".hs": "haskell",
    ".pl": "perl",
}

SKIP_PATTERNS = [
    "node_modules", "__pycache__", "venv", ".venv", ".next", "dist",
    "build", "target", "out", ".git", "vendor", ".min.js", ".bundle.js",
    "__MACOSX", ".DS_Store",
]


def discover_files(repo_path: str) -> list:
    files = []
    for ext, lang in EXT_LANG.items():
        for f in Path(repo_path).rglob(f"*{ext}"):
            if not any(x in str(f) for x in SKIP_PATTERNS):
                files.append((str(f), lang))
    return files


def cmd_index(args):
    """Index a codebase into DuckDB graph."""
    repo_path = str(Path(args.path).resolve())
    output_dir = args.output or os.path.join(repo_path, ".cgc-index")
    os.makedirs(output_dir, exist_ok=True)

    print(f"CGC Wiki Indexer v0.7.0")
    print(f"Repository: {repo_path}")
    print()

    # Discover files
    files = discover_files(repo_path)
    print(f"Files discovered: {len(files)}")

    from collections import Counter
    lang_counts = Counter(l for _, l in files)
    for lang, cnt in lang_counts.most_common():
        print(f"  {lang}: {cnt}")

    # Parse + resolve
    from codegraphcontext._cgc_rust import parse_and_prescan, resolve_call_groups, resolve_inheritance

    t0 = time.time()
    specs = [(f, l, False) for f, l in files]
    results, imports_map = parse_and_prescan(specs)
    valid = [r for r in results if "error" not in r]
    call_groups = resolve_call_groups(valid, imports_map, False)
    inheritance, _ = resolve_inheritance(valid, imports_map)
    t_parse = time.time() - t0
    print(f"\nParse + resolve: {t_parse:.1f}s")

    # Detect flows, routes, rationale
    from codegraphcontext.tools.indexing.execution_flows import detect_execution_flows
    from codegraphcontext.tools.indexing.route_extraction import extract_routes
    from codegraphcontext.tools.indexing.rationale_extraction import extract_rationales

    flows = detect_execution_flows(valid, call_groups)
    routes = extract_routes(valid, repo_path)
    rationales = extract_rationales(valid, repo_path)

    # Write to DuckDB
    from codegraphcontext.tools.indexing.persistence.duckdb_writer import DuckDBGraphWriter

    db_path = os.path.join(output_dir, "graph.duckdb")
    t0 = time.time()
    writer = DuckDBGraphWriter(db_path)
    counts = writer.write_all(valid, repo_path, call_groups, inheritance)
    writer.close()
    t_write = time.time() - t0

    # Summary
    funcs = counts.get("functions", 0)
    classes = counts.get("classes", 0)
    calls = counts.get("calls", 0)
    n_flows = counts.get("execution_flows", 0)
    n_routes = counts.get("routes", 0)
    n_rationale = counts.get("rationales", 0)

    print(f"DB write: {t_write:.1f}s")
    print(f"\n{'='*50}")
    print(f"INDEX COMPLETE")
    print(f"{'='*50}")
    print(f"  Functions:       {funcs}")
    print(f"  Classes:         {classes}")
    print(f"  CALLS edges:     {calls}")
    print(f"  Execution flows: {n_flows}")
    print(f"  API routes:      {n_routes}")
    print(f"  Rationale notes: {n_rationale}")
    print(f"  Output:          {db_path}")
    print(f"  Total time:      {t_parse + t_write:.1f}s")

    # Generate report
    report_path = os.path.join(output_dir, "GRAPH_REPORT.md")
    _generate_report(writer, db_path, repo_path, report_path, counts, flows, routes, rationales)
    print(f"  Report:          {report_path}")


def _generate_report(writer, db_path, repo_path, report_path, counts, flows, routes, rationales):
    """Generate GRAPH_REPORT.md (like Graphify)."""
    from codegraphcontext.tools.indexing.persistence.duckdb_writer import DuckDBGraphWriter

    w = DuckDBGraphWriter(db_path)
    top = w.get_top_connected(limit=10)
    stats = w.get_stats()
    w.close()

    repo_name = Path(repo_path).name
    lines = [
        f"# Graph Report — {repo_name}",
        "",
        f"## Summary",
        f"- **{counts.get('files', 0)}** files · **{counts.get('functions', 0)}** functions · **{counts.get('classes', 0)}** classes",
        f"- **{counts.get('calls', 0)}** call edges · **{counts.get('execution_flows', 0)}** execution flows",
        f"- **{counts.get('routes', 0)}** API routes · **{counts.get('rationales', 0)}** design rationale comments",
        "",
        f"## Top Connected (God Nodes)",
        "",
    ]

    for i, fn in enumerate(top[:10], 1):
        lines.append(f"{i}. **{fn.get('name', '?')}** ({fn.get('path', '?')}) — {fn.get('call_count', 0)} connections")

    if routes:
        lines.extend(["", "## API Routes", ""])
        lines.append("| Method | Path | Handler | Framework |")
        lines.append("|--------|------|---------|-----------|")
        for r in routes[:30]:
            lines.append(f"| {r['method']} | `{r['path']}` | {r['handler']} | {r['framework']} |")
        if len(routes) > 30:
            lines.append(f"| ... | ... | {len(routes) - 30} more | ... |")

    if flows:
        lines.extend(["", "## Key Execution Flows", ""])
        for f in flows[:10]:
            steps = " → ".join(s["name"] for s in f["steps"][:6])
            if len(f["steps"]) > 6:
                steps += " → ..."
            lines.append(f"- **{f['name']}** ({f['step_count']} steps): {steps}")

    if rationales:
        lines.extend(["", "## Design Rationale (from source comments)", ""])
        for r in rationales:
            ctx = f" in `{r['context']}`" if r["context"] else ""
            lines.append(f"- **[{r['tag']}]**{ctx}: {r['text']}")
            lines.append(f"  — `{r['file']}:{r['line']}`")

    lines.extend([
        "",
        "---",
        f"*Generated by CGC v0.7.0 · {time.strftime('%Y-%m-%d %H:%M')}*",
    ])

    with open(report_path, "w") as fh:
        fh.write("\n".join(lines))


def cmd_report(args):
    """Show graph report."""
    repo_path = str(Path(args.path).resolve())
    report_path = os.path.join(repo_path, ".cgc-index", "GRAPH_REPORT.md")
    if os.path.exists(report_path):
        print(open(report_path).read())
    else:
        print(f"No report found. Run: python cgc-wiki.py index {args.path}")


def cmd_query(args):
    """Search symbols in the graph."""
    repo_path = str(Path(args.path).resolve())
    db_path = os.path.join(repo_path, ".cgc-index", "graph.duckdb")
    if not os.path.exists(db_path):
        print(f"No index found. Run: python cgc-wiki.py index {args.path}")
        return

    from codegraphcontext.tools.indexing.persistence.duckdb_writer import DuckDBGraphWriter
    w = DuckDBGraphWriter(db_path)
    results = w.search_symbols(args.query, limit=20)
    w.close()

    print(f"Results for '{args.query}':")
    for r in results:
        print(f"  [{r['type']}] {r['name']} ({r['path']}:{r['line_number']})")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="CGC Wiki — Index codebase + generate docs")
    sub = parser.add_subparsers(dest="command")

    p_index = sub.add_parser("index", help="Index a codebase")
    p_index.add_argument("path", help="Repository path")
    p_index.add_argument("--output", help="Output directory (default: <repo>/.cgc-index)")

    p_report = sub.add_parser("report", help="Show graph report")
    p_report.add_argument("path", help="Repository path")

    p_query = sub.add_parser("query", help="Search symbols")
    p_query.add_argument("path", help="Repository path")
    p_query.add_argument("query", help="Search term")

    args = parser.parse_args()
    if args.command == "index":
        cmd_index(args)
    elif args.command == "report":
        cmd_report(args)
    elif args.command == "query":
        cmd_query(args)
    else:
        parser.print_help()
