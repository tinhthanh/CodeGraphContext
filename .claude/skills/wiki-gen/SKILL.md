# /wiki — Generate Code Documentation Wiki

Generate comprehensive wiki documentation using pre-computed module contexts.
**Zero LLM API cost** — uses your IDE's built-in AI.

## When to use

User types `/wiki` or asks "generate wiki", "document this codebase", "create docs"

## Prerequisites

If `.cgc-index/module_contexts/` doesn't exist, run first:
```bash
wiki-forge init
```

## Step 1: Read the index + plan

1. Read `.cgc-index/module_contexts/index.md` to see all modules
2. Count total modules
3. Create `wiki-output/` directory
4. If **≤ 10 modules**: process sequentially (Step 2A)
5. If **> 10 modules**: split into chunks of 8-10 and dispatch parallel agents (Step 2B)

## Step 2A: Sequential generation (≤ 10 modules)

For each module in index.md:
1. Read `.cgc-index/module_contexts/{slug}.md`
2. Follow "Instructions for AI Wiki Generator" at the bottom
3. Write `wiki-output/{slug}.md` with required sections
4. Move to next module

## Step 2B: Parallel subagent dispatch (> 10 modules)

**Dispatch ALL subagents in a SINGLE message** — this is the only way to run them in parallel.

Split modules into chunks of 8-10. Example for 30 modules:
```
[Agent tool call 1: modules 1-10, subagent_type="general-purpose"]
[Agent tool call 2: modules 11-20, subagent_type="general-purpose"]
[Agent tool call 3: modules 21-30, subagent_type="general-purpose"]
```
All three in ONE message. NOT three separate messages.

Each subagent prompt:

```
Read these module context files from `.cgc-index/module_contexts/` and generate wiki docs in `wiki-output/`:
[MODULE_LIST]

For EACH module:
1. Read `.cgc-index/module_contexts/{slug}.md`
2. Write `wiki-output/{slug}.md` with sections: Purpose, Architecture (Mermaid), API Endpoints (table), Execution Flows (steps), Design Decisions, Dependencies
3. Keep each doc 50-150 lines. Be concise but complete.
4. DO NOT write scripts. Read context and generate with AI reasoning.
```

**After subagents complete:** list `wiki-output/`, check for missing/short files, regenerate if needed.

## Step 3: Generate overview

After ALL module docs exist, generate `wiki-output/overview.md`:
1. Read `.cgc-index/GRAPH_REPORT.md` for god nodes + summary
2. Write:
   - Project summary + tech stack
   - Mermaid architecture diagram (max 10 nodes)
   - Module index table with links
   - God nodes (most connected functions)
   - API surface summary

## Required sections per doc

- **Purpose** — what this module does and why
- **Architecture** — key classes, Mermaid diagram
- **API Endpoints** — table: Method | Path | Description (skip if 0 routes)
- **Execution Flows** — step-by-step for top 2-3 flows
- **Design Decisions** — from [IMPORTANT], [WARNING], [NOTE] comments
- **Dependencies** — incoming/outgoing module references

## Quality rules

- 50-150 lines per doc
- Use confidence tags: EXTRACTED = certain (AST), INFERRED = cross-file
- DO NOT generate filler text or repetitive adverbs
- Include Mermaid diagrams where architecture is non-trivial
- Link between modules: `[Module Name](slug.md)`
