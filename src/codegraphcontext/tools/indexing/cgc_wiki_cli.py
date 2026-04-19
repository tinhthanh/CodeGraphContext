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

from codegraphcontext.tools.indexing.constants import DEFAULT_IGNORE_PATTERNS

import pathspec
_IGNORE_SPEC = pathspec.PathSpec.from_lines("gitwildmatch", DEFAULT_IGNORE_PATTERNS)


def discover_files(repo_path: str) -> list:
    root = Path(repo_path).resolve()
    files = []
    for ext, lang in EXT_LANG.items():
        for f in root.rglob(f"*{ext}"):
            rel = f.relative_to(root).as_posix()
            if not _IGNORE_SPEC.match_file(rel):
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

    # Write to DuckDB (detects flows/routes/rationales/op_params internally)
    from codegraphcontext.tools.indexing.persistence.duckdb_writer import DuckDBGraphWriter

    db_path = os.path.join(output_dir, "graph.duckdb")
    t0 = time.time()
    writer = DuckDBGraphWriter(db_path)
    counts = writer.write_all(valid, repo_path, call_groups, inheritance)
    t_write = time.time() - t0

    # Read back from DB (no duplicate extraction)
    flows = writer.get_execution_flows()
    routes = writer.get_routes()
    rationales = writer.get_rationales()
    op_params = writer.get_operational_params()
    writer.close()

    # Summary
    funcs = counts.get("functions", 0)
    classes = counts.get("classes", 0)
    calls = counts.get("calls", 0)
    n_flows = counts.get("execution_flows", 0)
    n_routes = counts.get("routes", 0)
    n_rationale = counts.get("rationales", 0)
    n_op_params = counts.get("operational_params", 0)

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
    print(f"  Op params:       {n_op_params}")
    print(f"  Output:          {db_path}")
    print(f"  Total time:      {t_parse + t_write:.1f}s")

    # Generate report
    report_path = os.path.join(output_dir, "GRAPH_REPORT.md")
    _generate_report(writer, db_path, repo_path, report_path, counts, flows, routes, rationales, op_params)
    print(f"  Report:          {report_path}")

    # Generate module contexts for AI IDE wiki generation
    from codegraphcontext.tools.indexing.module_grouping import auto_group_modules, group_parents
    from codegraphcontext.tools.indexing.context_generator import generate_module_contexts

    modules = auto_group_modules(valid, repo_path)
    parent_groups = group_parents(modules)
    ctx_dir = os.path.join(output_dir, "module_contexts")
    n_ctx = generate_module_contexts(
        modules, valid, repo_path, ctx_dir,
        call_groups=call_groups, routes=routes, flows=flows,
        rationales=rationales, op_params=op_params,
    )

    # Save modules.json with parent hierarchy
    import json as _json
    modules_json_path = os.path.join(output_dir, "modules.json")
    with open(modules_json_path, "w") as fh:
        _json.dump({"modules": modules, "parent_groups": parent_groups}, fh, indent=2)

    print(f"  Modules:         {len(modules)} ({n_ctx} context files)")
    if parent_groups:
        print(f"  Parent groups:   {len(parent_groups)}")
    print(f"  Context dir:     {ctx_dir}")
    print(f"\n  → AI IDE: read module_contexts/ + type /wiki to generate docs")


def _generate_report(writer, db_path, repo_path, report_path, counts, flows, routes, rationales, op_params=None):
    """Generate GRAPH_REPORT.md (like Graphify)."""
    from codegraphcontext.tools.indexing.persistence.duckdb_writer import DuckDBGraphWriter
    from codegraphcontext.tools.indexing.noise_filter import is_noise_node

    w = DuckDBGraphWriter(db_path)
    raw_top = w.get_top_connected(limit=50)
    top = [t for t in raw_top
           if not is_noise_node(t.get("name", ""), t.get("call_count", 0))][:10]
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

    if op_params:
        lines.extend(["", "## Operational Parameters", ""])
        lines.append("| Name | Value | Category | File | Line |")
        lines.append("|------|-------|----------|------|------|")
        for p in (op_params or [])[:30]:
            lines.append(
                f"| `{p['name']}` | `{p['value']}` | {p['category']} "
                f"| `{p['path']}` | {p['line_number']} |"
            )

    lines.extend([
        "",
        "---",
        f"*Generated by CGC v0.9.6 · {time.strftime('%Y-%m-%d %H:%M')}*",
    ])

    with open(report_path, "w") as fh:
        fh.write("\n".join(lines))


def cmd_search_index(args):
    """Build search index from wiki-output/ for AI navigation."""
    repo_path = str(Path(args.path).resolve())
    wiki_dir = os.path.join(repo_path, "wiki-output")
    if not os.path.exists(wiki_dir):
        print(f"No wiki-output/ found. Generate wiki first: /wiki in AI IDE")
        return

    cgc_dir = os.path.join(repo_path, ".cgc-index")
    index_path = os.path.join(cgc_dir, "WIKI_INDEX.md")

    lines = [
        "# Wiki Search Index",
        "",
        "Use this index to find the right wiki document for any question.",
        "",
        "| Document | Topics | Key Functions | Routes |",
        "|----------|--------|---------------|--------|",
    ]

    for md_file in sorted(Path(wiki_dir).glob("*.md")):
        name = md_file.stem
        content = md_file.read_text(encoding="utf-8", errors="replace")

        # Extract topics from headings
        headings = [line.lstrip("#").strip() for line in content.split("\n")
                    if line.startswith("#") and len(line) < 80][:5]
        topics = ", ".join(headings[:3])

        # Extract function names (backtick-wrapped)
        import re
        funcs = re.findall(r"`(\w+)\(\)`", content)
        funcs_str = ", ".join(sorted(set(funcs))[:5])

        # Extract routes
        routes = re.findall(r"(GET|POST|PUT|DELETE|PATCH)\s+`?(/[^\s`|]+)", content)
        routes_str = ", ".join(f"{m} {p}" for m, p in routes[:3])

        lines.append(f"| [{name}](wiki-output/{name}.md) | {topics} | {funcs_str} | {routes_str} |")

    lines.extend([
        "",
        "## How to use",
        "",
        "When answering a question about this codebase:",
        "1. Search this table for matching topics/functions/routes",
        "2. Read the matching wiki document(s)",
        "3. Answer using the wiki content (don't grep raw source)",
    ])

    os.makedirs(cgc_dir, exist_ok=True)
    with open(index_path, "w") as fh:
        fh.write("\n".join(lines))
    print(f"Search index: {index_path}")
    print(f"Documents indexed: {len(list(Path(wiki_dir).glob('*.md')))}")


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

### When answering questions about this codebase:
1. First check if `wiki-output/` exists — if yes, read `wiki-output/overview.md` then search relevant docs
2. If no wiki, read `.cgc-index/GRAPH_REPORT.md` for god nodes, routes, flows
3. Use graph data instead of grepping raw files

### Commands:
- `/wiki` — generate wiki documentation (reads module_contexts/, writes wiki-output/)
- `wiki-forge init --no-llm .` — rebuild index after code changes (or `cgc-wiki index .`)
- `cgc-wiki search-index .` — rebuild search index after wiki generation
- `cgc-wiki query . "search term"` — search symbols in the graph
"""


def _agents_md_section() -> str:
    return """## cgc-wiki
This project has a CGC knowledge graph at `.cgc-index/`.
Rules:
- Before answering architecture questions, read `.cgc-index/GRAPH_REPORT.md`
- The report contains: god nodes, API routes, execution flows, design rationale
- After modifying code, run `wiki-forge init --no-llm .` to rebuild (or `cgc-wiki index .`)
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

    # Remove any existing cgc-wiki hooks (old or new format)
    pre_tool[:] = [
        hook for hook in pre_tool
        if "cgc-wiki" not in str(hook.get("command", ""))
        and "cgc-wiki" not in str(hook.get("hooks", []))
    ]

    # Add with correct format
    pre_tool.append({
        "matcher": "Glob|Grep|Read",
        "hooks": [
            {
                "type": "command",
                "command": 'test -f .cgc-index/GRAPH_REPORT.md && echo "cgc-wiki: Knowledge graph exists at .cgc-index/. Read GRAPH_REPORT.md for god nodes, API routes, and execution flows before searching raw files." || true',
            }
        ],
    })

    settings_path.write_text(json.dumps(settings, indent=2))


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

p_search = sub.add_parser("search-index", help="Build search index from wiki-output/")
p_search.add_argument("path", nargs="?", default=".", help="Repository path")


def main():
    """Entry point for cgc-wiki CLI."""
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
    elif args.command == "search-index":
        cmd_search_index(args)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
