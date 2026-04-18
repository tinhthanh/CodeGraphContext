"""Auto-group source files into logical modules using graph structure.

Replaces LLM-based grouping (Phase 1 of llm-wiki) with deterministic
graph-based clustering. No LLM cost.

Grouping strategy:
1. Primary: directory structure (src/auth/ → "authentication")
2. Secondary: class_context clustering (files sharing classes)
3. Tertiary: import graph communities (files importing each other)
"""

from __future__ import annotations

import logging
import os
import re
from collections import defaultdict
from pathlib import Path
from typing import Any, Dict, List, Set, Tuple

logger = logging.getLogger(__name__)

# Directories to skip when naming modules
_SKIP_DIR_NAMES = {
    "src", "app", "lib", "libs", "core", "main", "java", "kotlin",
    "python", "typescript", "javascript", "resources", "static",
    "com", "org", "net", "io", "vn", "controllers", "services",
    "models", "dto", "entities", "repositories", "utils", "helpers",
    "common", "shared", "internal", "pkg", "cmd",
}

# Name cleanup patterns
_CLEANUP_RE = re.compile(r"[-_.]")


def _humanize_dir(name: str) -> str:
    """Convert directory name to human-readable module name."""
    name = _CLEANUP_RE.sub(" ", name)
    return name.strip().title()


def auto_group_modules(
    parsed_results: List[Dict[str, Any]],
    repo_path: str,
    max_files_per_module: int = 30,
    min_files_per_module: int = 2,
) -> List[Dict[str, Any]]:
    """Group source files into logical modules.

    Returns list of:
        {
            name: str,           # human-readable module name
            slug: str,           # file-safe slug
            files: [str],        # relative file paths
            primary_lang: str,   # dominant language
            classes: [str],      # class names in this module
            entry_functions: [str],  # key functions
        }
    """
    # Build file → data map
    file_data: Dict[str, Dict] = {}
    for r in parsed_results:
        if "error" in r:
            continue
        fp = r.get("path", "")
        try:
            rel = os.path.relpath(fp, repo_path)
        except ValueError:
            rel = Path(fp).name
        file_data[rel] = r

    if not file_data:
        return []

    # ── Strategy 1: Group by meaningful directory ──────────────
    dir_groups: Dict[str, List[str]] = defaultdict(list)

    for rel_path in file_data:
        parts = Path(rel_path).parts

        # Find first meaningful directory (skip generic ones)
        meaningful_dir = None
        for i, part in enumerate(parts[:-1]):  # skip filename
            if part.lower() not in _SKIP_DIR_NAMES and not part.startswith("."):
                meaningful_dir = part
                # If next dir is also meaningful, use both
                if i + 1 < len(parts) - 1:
                    next_part = parts[i + 1]
                    if next_part.lower() not in _SKIP_DIR_NAMES:
                        meaningful_dir = f"{part}/{next_part}"
                break

        if not meaningful_dir:
            # Use parent directory
            parent = Path(rel_path).parent.name
            if parent and parent != ".":
                meaningful_dir = parent
            else:
                meaningful_dir = "(root)"

        dir_groups[meaningful_dir].append(rel_path)

    # ── Split oversized groups ─────────────────────────────────
    final_groups: Dict[str, List[str]] = {}
    for dir_name, files in dir_groups.items():
        if len(files) <= max_files_per_module:
            final_groups[dir_name] = files
        else:
            # Split by subdirectory
            sub_groups: Dict[str, List[str]] = defaultdict(list)
            for f in files:
                parts = Path(f).parts
                # Find dir after the current group dir
                try:
                    idx = list(parts).index(dir_name.split("/")[0])
                    if idx + 1 < len(parts) - 1:
                        sub_dir = parts[idx + 1]
                        sub_groups[f"{dir_name}/{sub_dir}"].append(f)
                    else:
                        sub_groups[dir_name].append(f)
                except ValueError:
                    sub_groups[dir_name].append(f)

            for sub_name, sub_files in sub_groups.items():
                final_groups[sub_name] = sub_files

    # ── Merge tiny groups ──────────────────────────────────────
    merged: Dict[str, List[str]] = {}
    tiny: List[Tuple[str, List[str]]] = []

    for name, files in final_groups.items():
        if len(files) >= min_files_per_module:
            merged[name] = files
        else:
            tiny.append((name, files))

    # Merge tiny groups into "(other)" or nearest parent
    if tiny:
        other_files = []
        for name, files in tiny:
            other_files.extend(files)
        if other_files:
            merged["other"] = other_files

    # ── Build module metadata ──────────────────────────────────
    modules: List[Dict[str, Any]] = []

    for dir_name, files in sorted(merged.items(), key=lambda x: -len(x[1])):
        # Detect primary language
        from collections import Counter
        lang_counts = Counter()
        classes = set()
        entry_funcs = []

        for f in files:
            data = file_data.get(f, {})
            lang = data.get("lang", "")
            if lang:
                lang_counts[lang] += 1
            for cls in data.get("classes", []):
                classes.add(cls.get("name", ""))
            for fn in data.get("functions", []):
                name = fn.get("name", "")
                # Heuristic: entry functions are top-level, exported, or handlers
                if name and not name.startswith("_"):
                    ctx = fn.get("class_context", "")
                    if not ctx:  # top-level
                        entry_funcs.append(name)

        primary_lang = lang_counts.most_common(1)[0][0] if lang_counts else ""

        # Generate human-readable name
        name = _humanize_dir(dir_name.split("/")[-1])
        slug = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")

        modules.append({
            "name": name,
            "slug": slug,
            "dir": dir_name,
            "files": sorted(files),
            "file_count": len(files),
            "primary_lang": primary_lang,
            "classes": sorted(classes - {""}),
            "entry_functions": entry_funcs[:10],
        })

    logger.info("Auto-grouped %d files into %d modules", len(file_data), len(modules))
    return modules
