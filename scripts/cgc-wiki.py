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


def cmd_install(args):
    """Install CGC-Wiki skill + hooks for AI coding assistants."""
    platform = args.platform or _detect_platform()
    home = Path.home()
    skill_src = Path(__file__).parent.parent / ".claude" / "skills" / "wiki-gen" / "SKILL.md"

    print(f"Installing CGC-Wiki for: {platform}")

    if platform == "claude":
        # 1. Copy skill
        skill_dir = home / ".claude" / "skills" / "cgc-wiki"
        skill_dir.mkdir(parents=True, exist_ok=True)
        if skill_src.exists():
            (skill_dir / "SKILL.md").write_text(skill_src.read_text())
            print(f"  Skill → {skill_dir / 'SKILL.md'}")

        # 2. Add to CLAUDE.md
        claude_md = Path(".") / "CLAUDE.md"
        section = _claude_md_section()
        if claude_md.exists():
            content = claude_md.read_text()
            if "cgc-wiki" not in content:
                claude_md.write_text(content + "\n\n" + section)
                print(f"  CLAUDE.md updated")
            else:
                print(f"  CLAUDE.md already has cgc-wiki section")
        else:
            claude_md.write_text(section)
            print(f"  CLAUDE.md created")

        # 3. Install PreToolUse hook
        settings_path = home / ".claude" / "settings.json"
        _install_pretool_hook(settings_path)
        print(f"  PreToolUse hook → {settings_path}")

    elif platform == "cursor":
        rules_dir = Path(".") / ".cursor" / "rules"
        rules_dir.mkdir(parents=True, exist_ok=True)
        rule_file = rules_dir / "cgc-wiki.mdc"
        rule_file.write_text(_cursor_rule())
        print(f"  Rule → {rule_file}")

    elif platform == "codex":
        agents_md = Path(".") / "AGENTS.md"
        section = _agents_md_section()
        if agents_md.exists():
            content = agents_md.read_text()
            if "cgc-wiki" not in content:
                agents_md.write_text(content + "\n\n" + section)
        else:
            agents_md.write_text(section)
        print(f"  AGENTS.md updated")

    else:
        # Generic: write AGENTS.md
        agents_md = Path(".") / "AGENTS.md"
        section = _agents_md_section()
        if agents_md.exists():
            content = agents_md.read_text()
            if "cgc-wiki" not in content:
                agents_md.write_text(content + "\n\n" + section)
        else:
            agents_md.write_text(section)
        print(f"  AGENTS.md updated")

    print(f"\nDone! Now run: cgc-wiki index .")
    print(f"Then type /wiki in your AI assistant to generate docs.")


def cmd_hook(args):
    """Install/uninstall git hooks for auto-rebuild."""
    repo_path = Path(args.path or ".").resolve()
    git_dir = repo_path / ".git"
    if not git_dir.exists():
        print(f"Not a git repo: {repo_path}")
        return

    hooks_dir = git_dir / "hooks"
    hooks_dir.mkdir(exist_ok=True)
    hook_file = hooks_dir / "post-commit"

    if args.action == "install":
        script = f"""#!/bin/sh
# CGC-Wiki: auto-rebuild index after commit (AST only, no LLM cost)
if [ -d ".cgc-index" ]; then
    echo "[cgc-wiki] Rebuilding index..."
    python {Path(__file__).resolve()} index . 2>/dev/null &
fi
"""
        hook_file.write_text(script)
        hook_file.chmod(0o755)
        print(f"Post-commit hook installed: {hook_file}")
        print("Index will auto-rebuild after each commit (background, no LLM cost)")

    elif args.action == "uninstall":
        if hook_file.exists():
            content = hook_file.read_text()
            if "cgc-wiki" in content:
                hook_file.unlink()
                print(f"Hook removed: {hook_file}")
            else:
                print("Hook exists but not from cgc-wiki — skipping")
        else:
            print("No hook found")


def _detect_platform() -> str:
    """Auto-detect which AI coding platform is installed."""
    home = Path.home()
    if (home / ".claude").exists():
        return "claude"
    if (home / ".cursor").exists() or Path(".cursor").exists():
        return "cursor"
    if (home / ".codex").exists():
        return "codex"
    return "generic"


def _claude_md_section() -> str:
    return """## cgc-wiki
This project has a CGC knowledge graph at `.cgc-index/`.
Rules:
- Before answering architecture or codebase questions, read `.cgc-index/GRAPH_REPORT.md`
- Use the graph data (god nodes, routes, flows) instead of grepping raw files
- After modifying code, run `cgc-wiki index .` to keep the graph current
- Type `/wiki` to generate wiki documentation from the graph
"""


def _agents_md_section() -> str:
    return """## cgc-wiki
This project has a CGC knowledge graph at `.cgc-index/`.
Rules:
- Before answering architecture questions, read `.cgc-index/GRAPH_REPORT.md`
- The report contains: god nodes, API routes, execution flows, design rationale
- After modifying code, run `python cgc-wiki.py index .` to rebuild
"""


def _cursor_rule() -> str:
    return """---
description: CGC-Wiki code intelligence
alwaysApply: true
---

This project has a CGC knowledge graph at `.cgc-index/`.

Before answering architecture or codebase questions:
1. Read `.cgc-index/GRAPH_REPORT.md` for god nodes, routes, flows
2. Use the graph structure instead of grepping raw files
3. Type `/wiki` to generate wiki documentation
"""


def _install_pretool_hook(settings_path: Path):
    """Add PreToolUse hook to Claude Code settings."""
    settings_path.parent.mkdir(parents=True, exist_ok=True)

    settings = {}
    if settings_path.exists():
        try:
            settings = json.loads(settings_path.read_text())
        except (json.JSONDecodeError, OSError):
            pass

    hooks = settings.setdefault("hooks", {})
    pre_tool = hooks.setdefault("PreToolUse", [])

    # Check if already installed
    for hook in pre_tool:
        if "cgc-wiki" in str(hook.get("command", "")):
            return  # already installed

    pre_tool.append({
        "matcher": "Glob|Grep|Read",
        "command": 'test -f .cgc-index/GRAPH_REPORT.md && echo "cgc-wiki: Knowledge graph exists at .cgc-index/. Read GRAPH_REPORT.md for god nodes, API routes, and execution flows before searching raw files." || true',
    })

    settings_path.write_text(json.dumps(settings, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="CGC Wiki — Index codebase + generate docs",
        epilog="Examples:\n"
               "  cgc-wiki index .              Index current directory\n"
               "  cgc-wiki install               Install AI skill + hooks\n"
               "  cgc-wiki hook install           Auto-rebuild on git commit\n"
               "  cgc-wiki report .              Show graph report\n"
               "  cgc-wiki query . 'login'       Search symbols\n",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = parser.add_subparsers(dest="command")

    p_index = sub.add_parser("index", help="Index a codebase")
    p_index.add_argument("path", help="Repository path")
    p_index.add_argument("--output", help="Output directory (default: <repo>/.cgc-index)")

    p_install = sub.add_parser("install", help="Install AI skill + hooks")
    p_install.add_argument("--platform", choices=["claude", "cursor", "codex", "generic"],
                           help="Target platform (auto-detected if omitted)")

    p_hook = sub.add_parser("hook", help="Install/uninstall git hooks")
    p_hook.add_argument("action", choices=["install", "uninstall"])
    p_hook.add_argument("path", nargs="?", default=".", help="Repository path")

    p_report = sub.add_parser("report", help="Show graph report")
    p_report.add_argument("path", help="Repository path")

    p_query = sub.add_parser("query", help="Search symbols")
    p_query.add_argument("path", help="Repository path")
    p_query.add_argument("query", help="Search term")

    args = parser.parse_args()
    if args.command == "index":
        cmd_index(args)
    elif args.command == "install":
        cmd_install(args)
    elif args.command == "hook":
        cmd_hook(args)
    elif args.command == "report":
        cmd_report(args)
    elif args.command == "query":
        cmd_query(args)
    else:
        parser.print_help()
