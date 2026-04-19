"""Auto-group source files into logical modules using graph structure.

Replaces LLM-based grouping (Phase 1 of llm-wiki) with deterministic
graph-based clustering. No LLM cost.

Grouping strategy:
1. Primary: directory structure (src/auth/ → "authentication")
2. Secondary: class_context clustering (files sharing classes)
3. Tertiary: import graph communities (files importing each other)
"""

from __future__ import annotations

import json
import logging
import os
import re
from collections import defaultdict, Counter
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

    all_rel_paths = list(file_data.keys())

    # ── Strategy 0: Detect build manifest modules ─────────────
    # Maven: top-level dirs with pom.xml or build.gradle
    # Node: package.json workspaces
    manifest_modules: Dict[str, List[str]] = {}

    repo = Path(repo_path)
    # Check for Maven/Gradle multi-module
    for pom in repo.glob("*/pom.xml"):
        mod_dir = pom.parent.name
        manifest_modules[mod_dir] = []
    for gradle in repo.glob("*/build.gradle"):
        mod_dir = gradle.parent.name
        if mod_dir not in manifest_modules:
            manifest_modules[mod_dir] = []
    for gradle in repo.glob("*/build.gradle.kts"):
        mod_dir = gradle.parent.name
        if mod_dir not in manifest_modules:
            manifest_modules[mod_dir] = []

    # Check for Node.js workspaces (apps/*, libs/*, packages/*)
    pkg_json = repo / "package.json"
    if pkg_json.exists():
        try:
            pkg = json.loads(pkg_json.read_text())
            workspaces = pkg.get("workspaces", [])
            if isinstance(workspaces, dict):
                workspaces = workspaces.get("packages", [])
            for ws in workspaces:
                ws_base = ws.replace("/*", "").replace("*", "")
                for ws_dir in repo.glob(f"{ws_base}/*/package.json"):
                    mod_dir = str(ws_dir.parent.relative_to(repo))
                    manifest_modules[mod_dir] = []
        except (json.JSONDecodeError, OSError):
            pass

    # NX: check nx.json or project.json in subdirs
    for proj_json in repo.glob("*/project.json"):
        mod_dir = str(proj_json.parent.relative_to(repo))
        if mod_dir not in manifest_modules:
            manifest_modules[mod_dir] = []
    for proj_json in repo.glob("*/*/project.json"):
        mod_dir = str(proj_json.parent.relative_to(repo))
        if mod_dir not in manifest_modules:
            manifest_modules[mod_dir] = []

    # If manifest modules found, use them for grouping
    if manifest_modules:
        for rel_path in all_rel_paths:
            assigned = False
            for mod_dir in sorted(manifest_modules.keys(), key=len, reverse=True):
                if rel_path.startswith(mod_dir + "/") or rel_path.startswith(mod_dir + os.sep):
                    manifest_modules[mod_dir].append(rel_path)
                    assigned = True
                    break
            if not assigned:
                manifest_modules.setdefault("(root)", []).append(rel_path)

        # Remove empty modules, use TOP-LEVEL dir as slug (not "src")
        manifest_modules = {k: v for k, v in manifest_modules.items() if v}

        if manifest_modules:
            logger.info("Detected %d manifest-based modules: %s",
                        len(manifest_modules), list(manifest_modules.keys()))

            # Threshold above which a manifest-based module is sub-split by
            # the next directory level. Prevents single-NX-app monorepos
            # (e.g. apps/app with 1000+ files) from collapsing into one giant
            # module that wiki cannot navigate.
            _SUBSPLIT_THRESHOLD = max_files_per_module * 3  # e.g. 90

            def _common_prefix_len(file_list: List[str]) -> int:
                """Length (in path parts) of the common directory prefix of files."""
                if not file_list:
                    return 0
                split_parts = [Path(f).parts[:-1] for f in file_list]  # drop filename
                n = min(len(p) for p in split_parts)
                i = 0
                while i < n and all(p[i] == split_parts[0][i] for p in split_parts):
                    i += 1
                return i

            def _subsplit(files: List[str], skip_depth: int, depth: int = 0) -> Dict[str, List[str]]:
                """Split files by the first non-generic directory beyond skip_depth.
                Recursively sub-splits groups that are still above threshold (up to
                depth 3 to prevent explosion). Returns {sub_label -> files}."""
                groups: Dict[str, List[str]] = defaultdict(list)
                for f in files:
                    parts = Path(f).parts
                    start = skip_depth
                    sub = None
                    while start < len(parts) - 1:
                        candidate = parts[start]
                        if candidate.lower() in _SKIP_DIR_NAMES or candidate.startswith("."):
                            start += 1
                            continue
                        sub = candidate
                        break
                    if not sub:
                        sub = "(misc)"
                    groups[sub].append(f)

                # Recursively sub-split oversized groups (stop at depth 3)
                if depth < 3:
                    expanded: Dict[str, List[str]] = {}
                    for k, v in groups.items():
                        if len(v) > _SUBSPLIT_THRESHOLD and k != "(misc)":
                            # Find the actual common prefix length for this subgroup
                            # (could be deeper than the matched `k` if src/app wrappers exist)
                            subgroup_prefix = _common_prefix_len(v)
                            deeper = _subsplit(v, subgroup_prefix, depth + 1)
                            for dk, dv in deeper.items():
                                label = f"{k}/{dk}" if dk != "(misc)" else k
                                expanded[label] = dv
                        else:
                            expanded[k] = v
                    groups = expanded

                # Merge tiny sub-groups into (misc)
                merged: Dict[str, List[str]] = {}
                misc: List[str] = []
                for k, v in groups.items():
                    if len(v) >= min_files_per_module:
                        merged[k] = v
                    else:
                        misc.extend(v)
                if misc:
                    merged["(misc)"] = misc
                return merged

            modules = []
            for mod_dir, files in sorted(manifest_modules.items(), key=lambda x: -len(x[1])):
                mod_base = _humanize_dir(mod_dir.split("/")[0])
                mod_slug_base = re.sub(r"[^a-z0-9]+", "-", mod_dir.lower()).strip("-") or "root"

                if len(files) > _SUBSPLIT_THRESHOLD:
                    sub_groups = _subsplit(files, _common_prefix_len(files))
                    logger.info("Sub-splitting %s (%d files) into %d sub-modules",
                                mod_dir, len(files), len(sub_groups))
                    for sub_name, sub_files in sub_groups.items():
                        lang_counts = Counter(); classes = set()
                        for f in sub_files:
                            data = file_data.get(f, {})
                            if (lang := data.get("lang", "")): lang_counts[lang] += 1
                            for cls in data.get("classes", []):
                                classes.add(cls.get("name", ""))
                        sub_humanized = _humanize_dir(sub_name.split("/")[-1]) if sub_name != "(misc)" else "(misc)"
                        sub_slug_tail = re.sub(r"[^a-z0-9]+", "-", sub_name.lower()).strip("-") or "misc"
                        modules.append({
                            "name": f"{mod_base} / {sub_humanized}",
                            "slug": f"{mod_slug_base}-{sub_slug_tail}",
                            "dir": f"{mod_dir}/{sub_name}" if sub_name != "(misc)" else mod_dir,
                            "files": sorted(sub_files),
                            "file_count": len(sub_files),
                            "primary_lang": lang_counts.most_common(1)[0][0] if lang_counts else "",
                            "classes": sorted(classes - {""}),
                            "entry_functions": [],
                        })
                else:
                    lang_counts = Counter(); classes = set()
                    for f in files:
                        data = file_data.get(f, {})
                        if (lang := data.get("lang", "")): lang_counts[lang] += 1
                        for cls in data.get("classes", []):
                            classes.add(cls.get("name", ""))
                    modules.append({
                        "name": mod_base,
                        "slug": mod_slug_base,
                        "dir": mod_dir,
                        "files": sorted(files),
                        "file_count": len(files),
                        "primary_lang": lang_counts.most_common(1)[0][0] if lang_counts else "",
                        "classes": sorted(classes - {""}),
                        "entry_functions": [],
                    })

            return modules

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

    # ── Split oversized groups (recursive by directory depth) ──
    def _dir_split(files: List[str], after_dir: str, depth: int = 0) -> Dict[str, List[str]]:
        """Split files by the directory immediately after `after_dir` in each
        file's path. Recurses on oversized sub-groups (max depth 3)."""
        last_seg = after_dir.rsplit("/", 1)[-1]
        sub: Dict[str, List[str]] = defaultdict(list)
        for f in files:
            parts = Path(f).parts
            try:
                idx = len(parts) - 1 - list(reversed(parts)).index(last_seg)
            except ValueError:
                sub[after_dir].append(f)
                continue
            if idx + 1 < len(parts) - 1:
                sub_dir = parts[idx + 1]
                # Skip generic wrapper dirs transparently
                if sub_dir.lower() in _SKIP_DIR_NAMES and idx + 2 < len(parts) - 1:
                    sub_dir = parts[idx + 2]
                sub[f"{after_dir}/{sub_dir}"].append(f)
            else:
                sub[after_dir].append(f)
        if depth < 3:
            expanded: Dict[str, List[str]] = {}
            for k, v in sub.items():
                if len(v) > max_files_per_module * 3 and k != after_dir:
                    deeper = _dir_split(v, k, depth + 1)
                    expanded.update(deeper)
                else:
                    expanded[k] = v
            sub = expanded
        return dict(sub)

    final_groups: Dict[str, List[str]] = {}
    for dir_name, files in dir_groups.items():
        if len(files) <= max_files_per_module * 3:
            final_groups[dir_name] = files
        else:
            for sub_name, sub_files in _dir_split(files, dir_name).items():
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

    # ── Disambiguate duplicate slugs (Next.js app/ vs components/ etc.) ──
    # When two modules have same slug (e.g. src/app/admin and src/components/admin),
    # prepend parent dir segment so each gets a unique slug + distinguishable name.
    # Next.js route groups like (dashboard), (public) are kept (with parens stripped)
    # as meaningful disambiguators when they are the only parent segment available.
    slug_counts: Counter = Counter(m["slug"] for m in modules)
    for m in modules:
        if slug_counts[m["slug"]] > 1:
            raw_parts = [p for p in m["dir"].split("/") if p]
            # Strip parens from route groups but keep them as semantic parents
            cleaned_parts = [
                p[1:-1] if p.startswith("(") and p.endswith(")") else p
                for p in raw_parts
            ]
            if len(cleaned_parts) >= 2:
                full_slug = re.sub(r"[^a-z0-9]+", "-", "-".join(cleaned_parts).lower()).strip("-")
                parent_seg = _humanize_dir(cleaned_parts[-2])
                m["slug"] = full_slug
                m["name"] = f"{m['name']} ({parent_seg})"

    logger.info("Auto-grouped %d files into %d modules", len(file_data), len(modules))
    return modules


def _build_module_metadata(
    groups: Dict[str, List[str]],
    file_data: Dict[str, Dict],
    min_files: int = 2,
) -> List[Dict[str, Any]]:
    """Build module metadata from grouped files."""
    # Merge tiny groups
    merged: Dict[str, List[str]] = {}
    tiny_files = []
    for name, files in groups.items():
        if len(files) >= min_files:
            merged[name] = files
        else:
            tiny_files.extend(files)
    if tiny_files:
        merged["other"] = tiny_files

    modules = []
    for dir_name, files in sorted(merged.items(), key=lambda x: -len(x[1])):
        lang_counts = Counter()
        classes = set()
        for f in files:
            data = file_data.get(f, {})
            lang = data.get("lang", "")
            if lang:
                lang_counts[lang] += 1
            for cls in data.get("classes", []):
                classes.add(cls.get("name", ""))

        primary_lang = lang_counts.most_common(1)[0][0] if lang_counts else ""

        # Use full dir path for name (not just last segment)
        # e.g., "gateway-api/src" → "Gateway Api" with slug "gateway-api"
        dir_parts = dir_name.split("/")
        # For manifest modules, use top-level dir (gateway-api, service-pet, etc.)
        name_source = dir_parts[0] if dir_parts[0].lower() not in _SKIP_DIR_NAMES else dir_name
        name = _humanize_dir(name_source)
        slug = re.sub(r"[^a-z0-9]+", "-", name_source.lower()).strip("-")

        # Ensure unique slugs
        existing_slugs = {m["slug"] for m in modules}
        if slug in existing_slugs:
            if len(dir_parts) > 1:
                full = "-".join(dir_parts)
                slug = re.sub(r"[^a-z0-9]+", "-", full.lower()).strip("-")
                name = _humanize_dir(full)
            else:
                slug = f"{slug}-{len(modules)}"

        modules.append({
            "name": name,
            "slug": slug,
            "dir": dir_name,
            "files": sorted(files),
            "file_count": len(files),
            "primary_lang": primary_lang,
            "classes": sorted(classes - {""}),
            "entry_functions": [],
        })

    logger.info("Built %d modules from manifest", len(modules))
    return modules
