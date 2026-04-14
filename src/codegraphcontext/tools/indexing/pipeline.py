"""Orchestrates full-repo indexing (Tree-sitter path)."""

from __future__ import annotations

import asyncio
import time
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional

from ...core.jobs import JobManager, JobStatus
from ...utils.debug_log import debug_log, error_logger, info_logger
from .discovery import discover_files_to_index
from .persistence.writer import GraphWriter
from .pre_scan import pre_scan_for_imports
from .resolution.calls import build_function_call_groups
from .resolution.inheritance import build_inheritance_and_csharp_files

# Try to load Rust engine for accelerated parsing
try:
    from .engine import (
        RUST_AVAILABLE,
        _RUST_SUPPORTED_LANGS,
        _rust_parse_files_parallel,
        _rust_pre_scan,
        _rust_parse_and_prescan,
        _rust_resolve_calls,
        _rust_resolve_inheritance,
    )
except ImportError:
    RUST_AVAILABLE = False


async def run_tree_sitter_index_async(
    path: Path,
    is_dependency: bool,
    job_id: Optional[str],
    cgcignore_path: Optional[str],
    writer: GraphWriter,
    job_manager: JobManager,
    parsers: Dict[str, str],
    get_parser: Callable[[str], Any],
    parse_file: Callable[[Path, Path, bool], Dict[str, Any]],
    add_minimal_file_node: Callable[[Path, Path, bool], None],
) -> None:
    """Parse all discovered files, write symbols, then inheritance + CALLS.

    When the Rust engine is available, files with supported languages are
    parsed in parallel via Rust for significant speedup. Unsupported
    languages (e.g. Kotlin, Perl) fall back to the Python parser.
    """
    if job_id:
        job_manager.update_job(job_id, status=JobStatus.RUNNING)

    writer.add_repository_to_graph(path, is_dependency)
    repo_name = path.name

    files, _ignore_root = discover_files_to_index(path, cgcignore_path)

    if job_id:
        job_manager.update_job(job_id, total_files=len(files))

    all_file_data: List[Dict[str, Any]] = []
    resolved_repo_path_str = str(path.resolve()) if path.is_dir() else str(path.parent.resolve())
    repo_path_resolved = path.resolve() if path.is_dir() else path.parent.resolve()

    # --- Combined pre-scan + parsing phase ---
    if RUST_AVAILABLE:
        # Split files into Rust-supported and fallback
        rust_files = []
        fallback_files = []
        for file in files:
            if not file.is_file():
                continue
            lang = parsers.get(file.suffix)
            if lang and lang in _RUST_SUPPORTED_LANGS and file.suffix != ".ipynb":
                rust_files.append((file, lang))
            else:
                fallback_files.append(file)

        # Combined parse + pre-scan in one parallel pass (saves ~12% time)
        imports_map = {}
        if rust_files:
            info_logger(f"Rust combined parse+prescan for {len(rust_files)} files...")
            t_parse = time.time()
            specs = [(str(f), lang, is_dependency) for f, lang in rust_files]
            rust_results, imports_map = _rust_parse_and_prescan(specs)
            info_logger(f"Rust parse+prescan done in {time.time() - t_parse:.1f}s "
                        f"({len(imports_map)} symbols)")

            for file_data in rust_results:
                if "error" not in file_data:
                    file_data["repo_path"] = str(repo_path_resolved)
                    writer.add_file_to_graph(file_data, repo_name, imports_map, repo_path_str=resolved_repo_path_str)
                    all_file_data.append(file_data)

            if job_id:
                job_manager.update_job(job_id, processed_files=len(rust_files))

        # Fallback for unsupported files (.ipynb, Perl, etc.)
        processed_count = len(rust_files)
        for file in fallback_files:
            if job_id:
                job_manager.update_job(job_id, current_file=str(file))
            file_data = parse_file(repo_path_resolved, file, is_dependency)
            if "error" not in file_data:
                writer.add_file_to_graph(file_data, repo_name, imports_map, repo_path_str=resolved_repo_path_str)
                all_file_data.append(file_data)
            elif not file_data.get("unsupported"):
                add_minimal_file_node(file, repo_path_resolved, is_dependency)
            processed_count += 1

            if job_id:
                job_manager.update_job(job_id, processed_files=processed_count)
            if processed_count % 50 == 0:
                await asyncio.sleep(0)
    else:
        # Original Python-only path: separate pre-scan
        debug_log("Starting pre-scan to build imports map...")
        imports_map = pre_scan_for_imports(files, parsers.keys(), get_parser)
        debug_log(f"Pre-scan complete. Found {len(imports_map)} definitions.")
        processed_count = 0
        for file in files:
            if not file.is_file():
                continue
            if job_id:
                job_manager.update_job(job_id, current_file=str(file))
            repo_path = path.resolve() if path.is_dir() else file.parent.resolve()
            file_data = parse_file(repo_path, file, is_dependency)
            if "error" not in file_data:
                writer.add_file_to_graph(file_data, repo_name, imports_map, repo_path_str=resolved_repo_path_str)
                all_file_data.append(file_data)
            elif not file_data.get("unsupported"):
                add_minimal_file_node(file, repo_path, is_dependency)
            processed_count += 1

            if job_id:
                job_manager.update_job(job_id, processed_files=processed_count)
            if processed_count % 50 == 0:
                await asyncio.sleep(0)

    info_logger(
        f"File processing complete. {len(all_file_data)} files parsed. "
        f"Starting post-processing phase (inheritance + function calls)..."
    )

    # --- Post-processing phase ---
    t0 = time.time()

    if RUST_AVAILABLE:
        info_logger(f"[INHERITS] Rust-accelerated inheritance resolution ({len(all_file_data)} files)...")
        inheritance_batch, csharp_files = _rust_resolve_inheritance(all_file_data, imports_map)
    else:
        info_logger(f"[INHERITS] Resolving inheritance links across {len(all_file_data)} files...")
        inheritance_batch, csharp_files = build_inheritance_and_csharp_files(all_file_data, imports_map)
    writer.write_inheritance_links(inheritance_batch, csharp_files, imports_map)
    t1 = time.time()
    info_logger(f"Inheritance links created in {t1 - t0:.1f}s. Starting function calls...")

    if RUST_AVAILABLE:
        from ...cli.config_manager import get_config_value
        skip_external = (get_config_value("SKIP_EXTERNAL_RESOLUTION") or "false").lower() == "true"
        groups = _rust_resolve_calls(all_file_data, imports_map, skip_external)
    else:
        groups = build_function_call_groups(all_file_data, imports_map, None)
    writer.write_function_call_groups(*groups)
    t2 = time.time()
    info_logger(f"Function calls created in {t2 - t1:.1f}s. Total post-processing: {t2 - t0:.1f}s")

    if job_id:
        job_manager.update_job(job_id, status=JobStatus.COMPLETED, end_time=datetime.now())
