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
    output_dir = args.output or os.path.join(repo_path, "wiki", "raw")
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
    """Build search index from wiki/outputs/code/ for AI navigation."""
    repo_path = str(Path(args.path).resolve())
    wiki_dir = os.path.join(repo_path, "wiki", "outputs", "code")
    if not os.path.exists(wiki_dir):
        print(f"No wiki/outputs/code/ found. Generate wiki first: /wiki in AI IDE")
        return

    cgc_dir = os.path.join(repo_path, "wiki", "raw")
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

        lines.append(f"| [{name}](wiki/outputs/code/{name}.md) | {topics} | {funcs_str} | {routes_str} |")

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
    report_path = os.path.join(repo_path, "wiki", "raw", "GRAPH_REPORT.md")
    if os.path.exists(report_path):
        print(open(report_path).read())
    else:
        print(f"No report found. Run: python cgc-wiki.py index {args.path}")


def cmd_query(args):
    """Search symbols in the graph."""
    repo_path = str(Path(args.path).resolve())
    db_path = os.path.join(repo_path, "wiki", "raw", "graph.duckdb")
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
        skill_dir = home / ".claude" / "skills" / "wiki"
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

    elif platform == "antigravity":
        # 1. Skill
        skill_dir = Path(".") / ".agents" / "skills" / "wiki"
        skill_dir.mkdir(parents=True, exist_ok=True)
        (skill_dir / "SKILL.md").write_text(_antigravity_skill())
        print(f"  Skill → {skill_dir / 'SKILL.md'}")

        # 2. Rule
        rules_dir = Path(".") / ".agents" / "rules"
        rules_dir.mkdir(parents=True, exist_ok=True)
        (rules_dir / "cgc-wiki.md").write_text(_antigravity_rule())
        print(f"  Rule → {rules_dir / 'cgc-wiki.md'}")

        # 3. AGENTS.md (cross-tool)
        agents_md = Path(".") / "AGENTS.md"
        section = _agents_md_section()
        if agents_md.exists():
            content = agents_md.read_text()
            if "cgc-wiki" not in content:
                agents_md.write_text(content + "\n\n" + section)
                print(f"  AGENTS.md updated")
        else:
            agents_md.write_text(section)
            print(f"  AGENTS.md created")

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
if [ -d "wiki/raw" ]; then
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
    if (home / ".antigravity").exists() or Path(".agents").exists():
        return "antigravity"
    if (home / ".cursor").exists() or Path(".cursor").exists():
        return "cursor"
    if (home / ".codex").exists():
        return "codex"
    return "generic"


def _claude_md_section() -> str:
    return """## cgc-wiki
This project has a CGC knowledge graph at `wiki/raw/`.

### When answering questions about this codebase:
1. First check if `wiki/outputs/code/` exists — if yes, read `wiki/outputs/code/overview.md` then search relevant docs
2. If no wiki, read `wiki/raw/GRAPH_REPORT.md` for god nodes, routes, flows
3. Use graph data instead of grepping raw files

### Commands:
- `/wiki` — generate wiki documentation (reads module_contexts/, writes wiki/outputs/code/)
- `wiki-forge init` — rebuild index after code changes
"""


def _agents_md_section() -> str:
    return """## cgc-wiki
This project has a CGC knowledge graph at `wiki/raw/`.
Rules:
- Before answering architecture questions, read `wiki/raw/GRAPH_REPORT.md`
- The report contains: god nodes, API routes, execution flows, design rationale
- After modifying code, run `wiki-forge init` to rebuild
"""


def _cursor_rule() -> str:
    return """---
description: CGC-Wiki code intelligence
alwaysApply: true
---

This project has a CGC knowledge graph at `wiki/raw/`.

Before answering architecture or codebase questions:
1. Read `wiki/raw/GRAPH_REPORT.md` for god nodes, routes, flows
2. Use the graph structure instead of grepping raw files
3. Type `/wiki` to generate wiki documentation
"""


def _antigravity_skill() -> str:
    return """---
name: wiki
description: Generate code documentation wiki from CGC knowledge graph
trigger: user asks to generate wiki, document codebase, create docs, or types /wiki
---

# /wiki — Generate Code Documentation Wiki

Generate comprehensive wiki documentation using pre-computed module contexts.
**Zero LLM API cost** — uses your IDE's built-in AI.

## CRITICAL RULES

- **DO NOT write shell scripts, Python scripts, or call external CLIs.** YOU and your subagents must read context files and write docs using AI reasoning.
- **DO NOT write scripts. Read each context file and write each doc with AI reasoning.**
- Each doc must have real architectural analysis with Mermaid diagrams.
- Keep each doc **50-150 lines** max.

### Writing Style — MANDATORY

**WRITE LIKE AN ENGINEER, NOT A MARKETER.**

Every sentence must contain a concrete fact: a function name, a file path, a config value, a data flow, or a constraint. If a sentence could apply to any random project, delete it.

**BANNED patterns** (your output will be rejected if these appear):
- Adverb stuffing: "natively", "securely", "dynamically", "accurately", "cleanly", "perfectly", "effectively", "flawlessly", "structurally", "organically", "robustly"
- Vague verbs: "manages", "handles", "orchestrates", "facilitates", "leverages" without saying WHAT or HOW
- Meaningless modifiers: "complex", "massive", "enormous", "heavy", "deep", "strict", "explicit" when not quantified
- Redundant clauses: "ensuring X while maintaining Y avoiding Z" chains

**GOOD example:**
> `WorkflowService.cancel(id, reason)` marks the workflow as cancelled in `workflows` table, sends cancellation emails to pending signers via `NotificationService`, and logs `WORKFLOW_CANCELLED` audit event.

**BAD example (DO NOT WRITE LIKE THIS):**
> The workflow module operates as the central REST nerve center orchestrating complex lifecycle management dynamically handling massive operations securely ensuring clean mappings natively.

### Language Rule

**Write in English by default.** If the user requests Vietnamese (or another language):
- Section headings: translate (e.g., "Mục đích", "Kiến trúc", "Luồng thực thi")
- Explanatory text: translate naturally
- **NEVER translate**: function names, file paths, config values, class names, variable names, CLI commands, error messages, HTTP methods/paths, library names
- **NEVER invent technical jargon in the target language.** Use the original English term with translated context. Example: "Hàm `getAuth()` khởi tạo Better Auth instance" — NOT "Hàm Truy Xuất Lõi Mạch Định Danh Bảo Vệ"
- If unsure how to translate a term, keep it in English

### Data Extraction — MANDATORY

You MUST extract and include these from the module context file. Do NOT summarize them away:

1. **Operational Parameters** — if the context has an "Operational Parameters" section, include ALL of them as a table
2. **Key Functions** — list the top 10 most important functions with file:line references
3. **Route Table** — if the module has API routes, list EVERY route as `Method | Path | Handler`
4. **Design Rationale** — copy ALL `[IMPORTANT]`, `[WARNING]`, `[NOTE]` items verbatim from source
5. **Call Graph Summary** — state total internal calls count and list top 5 outgoing dependencies

## Prerequisites

If `wiki/raw/module_contexts/` doesn't exist:
```bash
wiki-forge init
```

## Step 1: Read the index + plan chunks

1. Read `wiki/raw/module_contexts/index.md`
2. Count total modules
3. Create `wiki/outputs/code/` directory
4. If **≤ 10 modules**: process all yourself (go to Step 2A)
5. If **> 10 modules**: split into chunks of 8-10 modules each, grouped by related slugs (go to Step 2B)

## Step 2A: Sequential generation (≤ 10 modules)

For each module in index.md:
1. Read `wiki/raw/module_contexts/{slug}.md`
2. Write `wiki/outputs/code/{slug}.md` with sections below
3. Move to next module

## Step 2B: Parallel subagent dispatch (> 10 modules)

**Split into chunks of 8-10 modules. Dispatch ALL subagents in a SINGLE message.**

This is critical — if you make one Agent call, wait, then make another, they run sequentially. All Agent tool calls must be in ONE response for true parallelism.

Example for 30 modules → 3 chunks:
```
[Agent tool call 1: modules 1-10, subagent_type="general-purpose"]
[Agent tool call 2: modules 11-20, subagent_type="general-purpose"]
[Agent tool call 3: modules 21-30, subagent_type="general-purpose"]
```
All three in ONE message. NOT three separate messages.

**IMPORTANT:** Use `subagent_type="general-purpose"` — NOT `Explore` (read-only, cannot write files).

Each subagent receives this prompt (substitute MODULE_LIST and CHUNK_NUM):

```
You are a wiki documentation subagent (chunk CHUNK_NUM).

Read these module context files from `wiki/raw/module_contexts/` and generate wiki docs in `wiki/outputs/code/`:
MODULE_LIST

For EACH module:
1. Read `wiki/raw/module_contexts/{slug}.md` — contains all context needed
2. Follow "Instructions for AI Wiki Generator" at the bottom of the context file
3. Write `wiki/outputs/code/{slug}.md` with the required sections listed below

## Required sections per doc:

### 1. Purpose (2-3 sentences)
What this module does. State file count, function count, and class count from the Stats line.

### 2. Architecture (Mermaid diagram)
Copy and enhance the Mermaid diagram from the context. Add the key classes/services as nodes.

### 3. API Endpoints (table — skip if 0 routes)
Extract EVERY route from the context file. Format:

| Method | Path | Handler | Description |
|--------|------|---------|-------------|

### 4. Key Functions (table — top 10)
Pick the 10 most important functions from the "Key Functions" section:

| Function | File:Line | Purpose |
|----------|-----------|---------|

### 5. Execution Flows (top 2-3)
From the "Execution Flows" section, describe what happens step-by-step. Include the step count and depth.

### 6. Design Rationale
Copy ALL [IMPORTANT], [WARNING], [NOTE], [CONTEXT] items from "Design Rationale" section.
Format as a bullet list with the tag, quote, and file:line reference. Do NOT paraphrase — keep the original wording.

### 7. Operational Parameters (table — skip if none)
If the context has "Operational Parameters", include ALL as:

| Name | Value | Category | File |
|------|-------|----------|------|

### 8. Call Graph Summary
- State: "N internal calls, M outgoing, K incoming" using actual numbers from context
- List top 5 outgoing calls with target file paths

### 9. Dependencies
- **Outgoing (Depends on)**: List with [[module-slug]] wikilinks
- **Incoming (Depended on by)**: List with [[module-slug]] wikilinks

## Writing rules — STRICTLY ENFORCED:

- 50-150 lines per doc. Be concise but data-rich.
- Every sentence must contain a concrete fact (function name, file path, config value, or data flow).
- BANNED words: "natively", "securely", "dynamically", "accurately", "cleanly", "perfectly", "effectively", "flawlessly", "structurally", "robustly", "orchestrates", "facilitates", "leverages"
- DO NOT write filler. If you don't know something, skip it. Short + correct > long + vague.
- Use confidence tags: EXTRACTED = certain (AST), INFERRED = cross-file resolution.
- When mentioning another module, use: [[module-slug]]
- Language: Write in English by default. If the user requested another language, translate headings and explanatory text but NEVER translate function names, file paths, config values, class names, HTTP paths. NEVER invent technical jargon.
```

**Step 2B-verify: Check results**

After all subagents complete:
- List `wiki/outputs/code/` directory
- Count generated files vs expected
- If any are missing, generate them yourself
- If any file is suspiciously short (< 20 lines), regenerate it
- **Quality spot-check**: Read 2-3 random files. If they contain banned adverbs or lack function/file references, regenerate them.

## Step 3: Generate overview

After ALL module docs exist, generate `wiki/outputs/code/overview.md`:
1. Read `wiki/raw/GRAPH_REPORT.md` for god nodes + summary stats
2. Read `wiki/raw/module_contexts/index.md` for the full module list with stats

Write overview with these REQUIRED sections:

### 3a. Project Summary (3-5 sentences)
What the project does, tech stack, total file/function/class counts.

### 3b. Architecture Diagram
Mermaid diagram showing the top-level layers (max 10 nodes). Label edges with what flows between them.

### 3c. Module Index Table
MUST be a markdown table with data from index.md:

| Module | Files | Functions | Classes | Key Feature |
|--------|-------|-----------|---------|-------------|

Link each module: `[Module Name](slug.md)`

### 3d. God Nodes
From GRAPH_REPORT.md, list the most-connected functions/classes (highest in-degree). These are the architectural hotspots.

### 3e. API Surface Summary
Group all routes by area (auth, workflows, admin, etc.) with total count per area.

### 3f. Design Decisions Summary
Collect the most critical [IMPORTANT] and [WARNING] items across all modules (top 5-10).
"""


def _antigravity_classify_skill() -> str:
    return '''---
name: wiki-classify
description: Classify wiki/outputs/code into entities, concepts, and sources for full vault structure
trigger: user asks to classify wiki, organize wiki, or types /wiki-classify
---

# /wiki-classify — Classify Wiki into Vault Structure

Read `wiki/outputs/code/` flat files and organize into `wiki/` vault structure with entities, concepts, and sources.
**Zero LLM API cost** — uses your IDE's built-in AI.

## ABSOLUTE PROHIBITION

**YOU MUST NOT write or execute ANY scripts.** No Python, no Bash, no Node.js, no `cat << EOF`, no `shutil.copy`. This skill is about YOU (the AI) reading files, understanding their content, and writing new markdown files using your reasoning.

If you write a script, the output will be garbage templates like "Initializes components mapping contexts" — which is useless. You MUST read the actual wiki/outputs/code content and extract real information.

**Violation examples (DO NOT DO THESE):**
- `cat << 'EOF' > scripts/wiki-classify.py`
- `python3 scripts/classify.py`
- `bash -c "for f in wiki/outputs/code/*.md; do cp ..."`
- Writing ANY .py, .sh, or .js file

**What you MUST do instead:**
- Read wiki/outputs/code/services.md with the Read tool
- Understand: "services uses Drizzle ORM for DB, pdf-lib for signing, Resend for email"
- Write wiki/entities/drizzle-orm.md with the Write tool, including real details you just read

## When to use

Run AFTER `/wiki` has generated `wiki/outputs/code/`. Creates the full vault structure that `wiki-forge serve` expects.

## Step 1: Read wiki/outputs/code

1. List all `.md` files in `wiki/outputs/code/`
2. Read `wiki/outputs/code/overview.md` to understand the project

## Step 2: Ensure wiki/ directories exist

Create: `wiki/sources/`, `wiki/entities/`, `wiki/concepts/`, `wiki/syntheses/`

## Step 3: Classify and distribute

### Sources (wiki/sources/)
Copy ALL wiki/outputs/code/*.md files to wiki/sources/ — these are code-to-doc modules.

### Entities (wiki/entities/)
Extract **external** technologies, libraries, services, and tools. NOT internal classes.

**What IS an entity:** Something you could Google independently — PostgreSQL, Next.js, EasyCA, VNeID, Zod.
**What is NOT an entity:** Internal service classes (WorkflowService, EmailService), generic terms (theme, middleware), or architectural patterns (those are concepts).

**Entity page format — every field must contain REAL data from wiki/outputs/code:**

```
---
title: {Entity Name}
type: entity
tags: [technology|library|service|database|tool]
sources: [[{originating-module}]]
---

# {Entity Name}

## What it is
{2-3 sentences: what this technology does in general.}

## How this project uses it
{3-5 sentences with concrete details FROM wiki/outputs/code modules you just read.}

**Key configuration:**
- {env var or config}: `{value}` — {purpose}

## Referenced in
- [[module-a]] — {specific role FROM the module doc you read}
- [[module-b]] — {specific role FROM the module doc you read}

## Related
- [[concept-x]] — {how they connect}
```

**Target: 15-30 entities.** Only EXTERNAL technologies in 2+ modules.

### Concepts (wiki/concepts/)
Extract architectural patterns, workflows, design principles. Include REAL implementation details.

**Concept page format — every field must contain REAL data from wiki/outputs/code:**

```
---
title: {Concept Name}
type: concept
tags: [pattern|workflow|architecture|security|process]
sources: [[{originating-module}]]
---

# {Concept Name}

## What it is
{2-3 sentences: general definition.}

## How this project implements it
{5-10 bullet points with REAL details from wiki/outputs/code:}
- {Which files/services implement it — from module docs you read}
- {Key functions with file:line — from Key Functions tables}
- {Config values, thresholds — from Operational Parameters}
- {[IMPORTANT]/[WARNING] notes — from Design Rationale sections}

## Flow
{Step by step with REAL function names from the wiki/outputs/code execution flows.}

## Related
- [[entity-a]], [[concept-b]]
- Modules: [[module-x]], [[module-y]]
```

**Priority concepts** (create if found):
- Multi-tenancy, Provider/DI pattern, Incremental PDF signing
- Impersonation, Audit logging, Magic link auth, Workflow state machine

**Target: 15-25 concepts.**

## Step 4: Generate wiki/overview.md and wiki/index.md

## Step 5: Report

## Rules
- **ABSOLUTELY NO SCRIPTS** — no Python, no Bash, no cat heredoc. Use Read/Write tools only.
- **DO NOT delete or modify wiki/outputs/code/**
- **DO NOT invent information** — only extract from wiki/outputs/code/ content
- **DO NOT use template phrases** — BANNED: "Initializes components mapping contexts", "Evaluates specific bounds", "Translates executions updating database layers", "Serves as an architectural backbone", "Contextually binds this principle", "Inherits standard operational routing constraints"
- Entity = external technology only (NOT internal classes)
- Concept = architectural pattern with REAL implementation details
- Use [[wikilinks]] to cross-reference
'''


def _antigravity_docs_index_skill() -> str:
    return """---
name: docs-index
description: Index docs/ folder into compact summaries that /wiki can reference
trigger: user asks to index docs, or types /docs-index
---

# /docs-index — Index Documentation for Wiki

Scan `docs/` folder, create compact summaries in `wiki/raw/docs-context/` so `/wiki` can merge hand-written docs into generated wiki.
**Zero LLM API cost** — uses your IDE's built-in AI.

## When to use

Run BEFORE `/wiki` if the project has a `docs/` folder with hand-written documentation (architecture, API references, guides, etc.).

Only needs to run once, or when docs/ content changes.

## Step 1: Discover docs

1. Check if `docs/` exists. If not, check `documentation/`, `doc/`. If none found, stop.
2. List all `.md` files in docs/ with file sizes
3. Create `wiki/raw/docs-context/` directory

## Step 2: Read module index

Read `wiki/raw/module_contexts/index.md` to get the list of code modules. This is needed to map docs → modules.

## Step 3: Process each doc

For each markdown file in `docs/`:

1. Read the full file
2. Write a compact summary to `wiki/raw/docs-context/{filename}` with this format:

```markdown
# {Document Title}

**Source:** `docs/{filename}` ({size} bytes)
**Related modules:** [[module-a]], [[module-b]], ...

## Key Information

- {Bullet point 1: concrete fact, number, or decision}
- {Bullet point 2}
- ...
- {Bullet point N}

## Data Tables (preserve verbatim)

{Copy any tables from the source doc — API endpoints, config values,
schema definitions, environment variables. These are high-value structured
data that must not be summarized away.}

## Constraints & Decisions

- {Any architectural decisions, limitations, or requirements mentioned}
```

### Rules for summarization:

- **Target: 30-80 lines per summary** (source may be 200-500 lines)
- **KEEP verbatim:** tables, code blocks, config values, API specs, env vars, schema definitions
- **KEEP verbatim:** anything marked IMPORTANT, WARNING, NOTE, TODO
- **Summarize:** narrative explanations, step-by-step guides, background context
- **Map to modules:** identify which `module_contexts/` modules this doc relates to using [[module-slug]] wikilinks
- **Skip:** table of contents, navigation links, badges, images

### For non-markdown files (PDF, DOCX):

- Note their existence in the index but do not attempt to read them
- Write: `**Format:** PDF (not indexed — use wiki-forge ingest for PDF/DOCX processing)`

## Step 4: Generate index

Write `wiki/raw/docs-context/index.md`:

```markdown
# Documentation Context Index

Summaries of hand-written docs for wiki generation.

| Document | Size | Related Modules | Key Topic |
|----------|------|-----------------|-----------|
| [system-architecture](system-architecture.md) | 32KB | [[services]], [[schema]], [[auth]] | System layers, data flows |
| [database-schema](database-schema.md) | 15KB | [[schema]], [[db]] | Table definitions, relations |
| ... | | | |

## Usage

These summaries are consumed by `/wiki` during wiki/outputs/code generation.
To regenerate: `/docs-index`
```

## Step 5: Report

```
Docs Index Summary
──────────────────
Source: docs/ ({N} files, {total_size})
Output: wiki/raw/docs-context/ ({N} summaries)
Skipped: {N} non-markdown files

Next step: run /wiki to generate wiki with docs context merged
```
"""


def _antigravity_review_skill() -> str:
    return """---
name: wiki-review
description: Review and fix wiki/outputs/code quality by comparing against CGC module contexts
trigger: user asks to review wiki, fix wiki quality, or types /wiki-review
---

# /wiki-review — Wiki Quality Review & Fix

Review `wiki/outputs/code/` docs against `wiki/raw/module_contexts/` source data. Fix quality issues in-place.
**Zero LLM API cost** — uses your IDE's built-in AI.

## When to use

Run AFTER `/wiki` has generated `wiki/outputs/code/`. This is a quality gate before committing or publishing.

## Step 1: Build checklist

Read `wiki/outputs/code/overview.md` and list all module doc files. For each file, you will check quality.

If there are more than 15 files, split into chunks of ~10 and dispatch subagents in parallel (same pattern as `/wiki`).

## Step 2: Review each doc

For each `wiki/outputs/code/{slug}.md`:

1. Read `wiki/outputs/code/{slug}.md` (the generated doc)
2. Read `wiki/raw/module_contexts/{slug}.md` (the source context)
3. Check against this checklist:

### Quality Checklist

| # | Check | How to verify |
|---|-------|---------------|
| 1 | **No filler adverbs** | Search for: "natively", "securely", "dynamically", "accurately", "cleanly", "perfectly", "effectively", "flawlessly", "structurally", "robustly". If found → rewrite the sentence with a concrete fact. |
| 2 | **No invented jargon** | If doc is in Vietnamese/other language: check that function names, file paths, class names, config values are in English. If translated → fix. |
| 3 | **Operational params preserved** | Compare "Operational Parameters" section in context vs doc. If context has params but doc doesn't → add them. |
| 4 | **Design rationale complete** | Count `[IMPORTANT]`, `[WARNING]`, `[NOTE]` items in context's "Design Rationale" section. Compare with doc. If any missing → add verbatim with file:line. |
| 5 | **Route table complete** | If context lists routes, check doc has ALL routes (not just "5 representative"). If truncated → expand to full list. |
| 6 | **Key functions listed** | Doc should have top 10 functions with file:line refs. If missing → add from context "Key Functions" section. |
| 7 | **Mermaid diagram present** | Every doc must have at least 1 Mermaid diagram. If missing → add based on context "Architecture" section. |
| 8 | **Line count 50-150** | If < 50 → too thin, add missing data. If > 150 → trim redundant text. |
| 9 | **Every sentence has a fact** | Each sentence should contain a function name, file path, config value, or data flow. Sentences like "This module handles various operations" → rewrite or delete. |

### Fix process

- If a file fails 1-2 checks: edit in-place (use Edit tool)
- If a file fails 3+ checks: rewrite entirely from the module context
- Track fixes in a summary

## Step 3: Review overview.md

Check `wiki/outputs/code/overview.md` specifically for:
1. Module index TABLE exists (not just bullet list)
2. God nodes section exists
3. API surface summary with route counts exists
4. Architecture Mermaid diagram has labeled edges
5. No filler text

Fix any issues.

## Step 4: Report

After all reviews, output a summary:

```
Wiki Review Summary
───────────────────
Files reviewed: N
Files passed: N
Files fixed: N
Files rewritten: N

Fixes applied:
- {slug}.md: added 3 missing operational params
- {slug}.md: removed filler adverbs (5 instances)
- {slug}.md: expanded route table from 5 to 27 routes
- overview.md: added module index table
```
"""


def _antigravity_rule() -> str:
    return """---
description: Use CGC wiki before searching raw files
alwaysApply: true
---

This project has a CGC knowledge graph at `wiki/raw/`.

Before answering architecture or codebase questions:
1. First check if `wiki/outputs/code/` exists — if yes, read `wiki/outputs/code/overview.md` then search relevant docs
2. If no wiki, read `wiki/raw/GRAPH_REPORT.md` for god nodes, routes, flows
3. Use graph data instead of grepping raw files

Commands:
- `/wiki` — generate wiki documentation (reads module_contexts/, writes wiki/outputs/code/)
- `wiki-forge init` — rebuild index after code changes
"""


def _antigravity_wiki_docs_skill() -> str:
    return """---
name: wiki-docs
description: Transform raw docs repo (.md files) into structured wiki pages with Summary, Key Claims, Connections
user_invocable: true
trigger: user asks to generate wiki from docs, process documentation repo, or types /wiki-docs
---

# /wiki-docs — Generate Wiki from Docs Repository

Transform raw markdown docs (Confluence exports, BA docs, design docs) into structured wiki pages.
**Zero LLM API cost** — uses your IDE's built-in AI.

## CRITICAL RULES

- **DO NOT write shell scripts or Python scripts.** Read files and write docs using AI reasoning only.
- **DO NOT copy-paste raw content.** Every output page must be a structured synthesis.
- **DO NOT hallucinate.** Every claim must come from the source file. No invented facts.
- **SKIP empty files** — files under 200 bytes or with only a metadata header are noise, skip them.

### Writing Style — MANDATORY

Every sentence must contain a concrete fact from the source document.

**BANNED patterns:**
- Vague summaries: "This document discusses various aspects of..."
- Filler: "comprehensively", "effectively", "seamlessly", "robustly"
- Invented content not in source file

**GOOD example:**
> The authentication flow has 3 layers: Traefik API gateway → authorization-service (token check) → Keycloak (token verify). Logout triggers a revoke token flow.

**BAD example:**
> This document comprehensively covers authentication aspects effectively managing security.

### Language Rule

Write in the same language as the source document. Never translate technical names, system names, or product names.

## Prerequisites

Run `wiki-forge init-docs .` first. This creates `wiki/sources/` with copied docs.

If `wiki/sources/` is empty, stop and tell the user to run `wiki-forge init-docs .`.

## Step 1: Discover source folders

1. List all subdirectories in `wiki/sources/`
2. For each folder, count `.md` files
3. Print inventory:
```
wiki/sources/
  confluence/architecture/   → 45 files
  confluence/pmo/            → 12 files
  context/                   → 8 files
  ...
```
4. Process folder-by-folder to avoid context overflow

## Step 2: Filter noise

For each `.md` file, skip if:
- File size < 200 bytes
- Content is ONLY a metadata header: `> Migrated from Confluence: ...`
- Title only with no body content

Log skipped files count per folder.

## Step 3: Process each file

For each non-empty source file:

1. Read the file
2. Strip Confluence metadata line: `> Migrated from Confluence: Space ... | Page ID: ... | Last updated: ...`
3. Generate structured output:

```markdown
---
title: "{Document Title}"
type: source
source_file: {relative path from wiki/sources/}
---

## Summary

{2-4 sentences. Concrete facts only. What this document describes, the key system/process/decision covered.}

## Key Claims

- **{Claim heading}**: {specific fact, number, decision, or constraint from the document}
- **{Claim heading}**: {specific fact}
- ... (3-8 bullet points, only what's explicitly stated in source)

## Connections

* [[{Entity or system name}]] — {1-line role description}
* [[{Another entity}]] — {role}
(Only list entities/systems explicitly mentioned in the source)
```

4. Overwrite the file with structured output

## Step 4: Process by folder in batches

Process folders one at a time. For large folders (>20 files), dispatch subagents in parallel:
- Each subagent handles ~10 files
- Subagent reads source, writes structured output

After each folder completes, print:
```
✓ confluence/architecture/ — 32 processed, 13 skipped (empty)
```

## Step 5: Update overview.md

After all folders processed, update `wiki/overview.md`:

```markdown
# {Project Name} — Documentation Overview

## Folders

| Folder | Files processed | Files skipped |
|--------|----------------|---------------|
| confluence/architecture/ | 32 | 13 |
| ...

## Total: {N} pages ready for wiki-forge pack
```

## Step 6: Final report

```
Wiki Docs Summary
─────────────────
Folders processed: N
Files processed:   N
Files skipped:     N (empty/noise)

Next: wiki-forge pack --vault .
```
"""


def _antigravity_wiki_docs_review_skill() -> str:
    return """---
name: wiki-docs-review
description: Review and fix wiki/sources/ quality for docs repos — check accuracy, remove hallucinations, fix empty pages
user_invocable: true
trigger: user asks to review docs wiki, check wiki-docs quality, or types /wiki-docs-review
---

# /wiki-docs-review — Docs Wiki Quality Review

Review `wiki/sources/` pages generated by `/wiki-docs`. Fix accuracy issues and quality problems.
**Zero LLM API cost** — uses your IDE's built-in AI.

## When to use

Run AFTER `/wiki-docs` has processed `wiki/sources/`. This is a quality gate before `wiki-forge pack`.

## Step 1: Build review checklist

List all `.md` files in `wiki/sources/` (recursively). Group by folder.
Total files to review. If > 30 files, dispatch subagents per folder in parallel.

## Step 2: Review each file

For each processed file in `wiki/sources/`:

1. Read the structured output file
2. Read the original source (check if raw content was in the same file before processing, or skip if already overwritten)

### Quality Checklist

| # | Check | How to verify |
|---|-------|---------------|
| 1 | **Has Summary section** | Must have `## Summary` with 2+ sentences. If missing → generate from content. |
| 2 | **Has Key Claims** | Must have `## Key Claims` with 3+ bullet points. If empty → extract from content. |
| 3 | **No filler language** | Search for: "comprehensively", "effectively", "seamlessly", "robustly", "various aspects". If found → rewrite with concrete facts. |
| 4 | **No hallucinated claims** | Each Key Claim must be traceable to content in the file. If a claim seems invented → remove it. |
| 5 | **Connections are real** | Each `[[Entity]]` in Connections must be mentioned in the file content. If not → remove. |
| 6 | **Not a copy-paste dump** | If Summary is just the first paragraph of the source verbatim → rewrite as synthesis. |
| 7 | **Line count 15-60** | If < 15 → too thin (likely empty source), flag for deletion. If > 60 → trim redundant content. |
| 8 | **Concrete facts present** | At least 3 Key Claims must have specific names, numbers, or decisions — not generic descriptions. |

### Fix process

- Fail 1-2 checks → edit in-place
- Fail 3+ checks → rewrite from scratch using file content
- File < 15 lines AND source was empty → delete the file, log as removed

## Step 3: Check overview.md

Verify `wiki/overview.md`:
1. Has folder table with file counts
2. Total count matches actual files
3. No filler intro text

## Step 4: Report

```
Wiki Docs Review Summary
─────────────────────────
Files reviewed:  N
Files passed:    N
Files fixed:     N
Files deleted:   N (empty sources)

Issues fixed:
- confluence/architecture/admin-service.md: deleted (empty source)
- confluence/pmo/sprint-planning.md: rewrote Key Claims (had filler)
- context/product-modules.md: removed 2 hallucinated Connections

Next: wiki-forge pack --vault .
```
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
                "command": 'test -f wiki/raw/GRAPH_REPORT.md && echo "cgc-wiki: Knowledge graph exists at wiki/raw/. Read GRAPH_REPORT.md for god nodes, API routes, and execution flows before searching raw files." || true',
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
p_index.add_argument("--output", help="Output directory (default: <repo>/wiki/raw)")

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

p_search = sub.add_parser("search-index", help="Build search index from wiki/outputs/code/")
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
