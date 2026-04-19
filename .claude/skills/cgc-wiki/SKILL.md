# CGC Code Wiki — AI Agent Skill

Generate comprehensive code documentation wiki from any codebase using CodeGraphContext (Rust-accelerated indexing) + LLM.

## What this does

1. **Index** codebase into a knowledge graph (functions, classes, calls, routes, flows)
2. **Generate** wiki documentation per module using LLM (with graph context)
3. **Output** structured markdown docs with architecture diagrams

## Prerequisites

```bash
pip install wiki-forge    # CGC bundled (Rust + DuckDB)
```

## Quick Start

```bash
# Index + install AI IDE hooks (no LLM, free, ~5-20s)
wiki-forge init --no-llm /path/to/repo

# Or use cgc-wiki directly
cgc-wiki index /path/to/repo
```

## Step-by-Step Guide for AI Agents

### Step 1: Index the codebase

```python
import sys
sys.path.insert(0, '/path/to/CodeGraphContext/src')
from pathlib import Path
from codegraphcontext._cgc_rust import parse_and_prescan, resolve_call_groups, resolve_inheritance
from codegraphcontext.tools.indexing.execution_flows import detect_execution_flows
from codegraphcontext.tools.indexing.route_extraction import extract_routes
from codegraphcontext.tools.indexing.rationale_extraction import extract_rationales
from codegraphcontext.tools.indexing.persistence.duckdb_writer import DuckDBGraphWriter

# Discover files
repo_path = "/path/to/repo"
EXT_LANG = {'.py':'python','.js':'javascript','.ts':'typescript','.tsx':'tsx','.java':'java','.go':'go'}
SKIP = ['node_modules','__pycache__','venv','.next','dist','target','.min.js','vendor']

files = []
for ext, lang in EXT_LANG.items():
    for f in Path(repo_path).rglob(f"*{ext}"):
        if not any(x in str(f) for x in SKIP):
            files.append((str(f), lang))

# Parse + resolve (Rust, parallel)
specs = [(f, lang, False) for f, lang in files]
results, imports_map = parse_and_prescan(specs)
valid = [r for r in results if 'error' not in r]
call_groups = resolve_call_groups(valid, imports_map, False)
inheritance, _ = resolve_inheritance(valid, imports_map)

# Detect execution flows + routes + rationale
flows = detect_execution_flows(valid, call_groups)
routes = extract_routes(valid, repo_path)
rationales = extract_rationales(valid, repo_path)

# Write to DuckDB
db_path = f"{repo_path}/.cgc-index/graph.duckdb"
writer = DuckDBGraphWriter(db_path)
counts = writer.write_all(valid, repo_path, call_groups, inheritance)
writer.close()

print(f"Indexed: {counts}")
print(f"Flows: {len(flows)}, Routes: {len(routes)}, Rationales: {len(rationales)}")
```

### Step 2: Query the graph

```python
writer = DuckDBGraphWriter(db_path)

# Top connected functions (architecture hubs)
top = writer.get_top_connected(limit=20)

# API routes (surface area)
routes = writer.get_routes(limit=100)

# Execution flows (what happens when X is called)
flows = writer.get_execution_flows(limit=50)

# Functions in a specific file
funcs = writer.get_functions_in_file("/path/to/file.ts")

# Call graph for specific files
edges = writer.get_call_graph_for_files(["/path/to/file1.ts", "/path/to/file2.ts"])

# Search symbols
results = writer.search_symbols("login", limit=10)

writer.close()
```

### Step 3: Generate wiki per module

For each module (group of related files):

```
## Prompt template for LLM:

Write documentation for the **{module_name}** module.

## Source Files
{file_list}

## Function Signatures
{signatures}

## Call Graph
Internal: {intra_calls}
Outgoing: {outgoing_calls}
Incoming: {incoming_calls}

## API Routes
{routes}

## Execution Flows
{flows}

## Design Rationale
{rationale_comments}

## Operational Parameters
{config_values}

Write comprehensive docs covering purpose, architecture, key components,
and how it connects to the rest of the codebase.
```

### Step 4: Output structure

```
wiki-output/
├── overview.md                  # Project overview + architecture diagram
├── api-routes.md                # All API endpoints
├── authentication.md            # Module: auth
├── user-management.md           # Module: users
├── data-layer.md                # Module: database/ORM
├── ...
└── parent-*.md                  # Category pages
```

## What CGC extracts (per file)

| Data | Source | Confidence |
|------|--------|-----------|
| Functions (name, args, line, complexity) | Tree-sitter AST | EXTRACTED |
| Classes (name, bases, methods) | Tree-sitter AST | EXTRACTED |
| Imports (module, alias) | Tree-sitter AST | EXTRACTED |
| Same-file calls (fn → fn) | Tree-sitter AST | EXTRACTED |
| Cross-file calls (fn → fn) | Resolution engine | INFERRED |
| Execution flows (entry → chain) | BFS on call graph | INFERRED |
| API routes (method, path, handler) | Decorator/source scan | EXTRACTED |
| Design rationale (NOTE/WHY/HACK) | Comment scan | EXTRACTED |
| Variables (name, type, line) | Tree-sitter AST | EXTRACTED |

## Supported languages (19)

Python, JavaScript, TypeScript, TSX, Java, Go, Rust, Ruby, C#, C, C++,
PHP, Kotlin, Scala, Swift, Haskell, Dart, Perl, Elixir

## Supported frameworks (routes)

Express.js, FastAPI, Flask, Spring Boot, NestJS, Next.js, Laravel,
Django, Go net/http, Gin, Echo

## Performance

| Repo size | Parse time | DB write | Total |
|-----------|-----------|---------|-------|
| 50 files | 0.2s | 0.4s | 0.6s |
| 300 files | 3s | 1s | 4s |
| 800 files | 5s | 2s | 7s |
| 1200 files | 15s | 4s | 19s |

## Tips

- Run index ONCE, query many times (graph persists in DuckDB)
- Use `--skip-git` for repos without .git directory
- Minified files (.min.js) are auto-skipped
- For incremental updates, re-run index on changed files only
- Graph data is 71x smaller than raw source → cheaper LLM prompts
