"""
Engine adapter: selects Rust or Python backend for parsing + resolution.

The Rust backend (_cgc_rust) provides significant performance improvements
for file parsing and parallel processing. If unavailable, falls back to
the existing Python implementation transparently.

Set CGC_USE_PYTHON_PARSER=1 to force Python backend for debugging.
"""
import os
import logging

logger = logging.getLogger(__name__)

RUST_AVAILABLE = False

if not os.environ.get("CGC_USE_PYTHON_PARSER"):
    try:
        from codegraphcontext._cgc_rust import (
            parse_file as _rust_parse_file,
            parse_files_parallel as _rust_parse_files_parallel,
            pre_scan_for_imports as _rust_pre_scan,
            resolve_call_groups as _rust_resolve_calls,
            resolve_inheritance as _rust_resolve_inheritance,
            sanitize_props as _rust_sanitize_props,
        )
        RUST_AVAILABLE = True
        logger.info("Rust parsing engine loaded successfully")
    except ImportError:
        logger.debug("Rust parsing engine not available, using Python fallback")


def parse_file(path, lang, is_dependency=False, index_source=False,
               parser_wrapper=None, **kwargs):
    """Parse a single file. Uses Rust if available, else Python."""
    if RUST_AVAILABLE and lang in _RUST_SUPPORTED_LANGS:
        return _rust_parse_file(str(path), lang, is_dependency, index_source)

    # Python fallback
    if parser_wrapper is None:
        raise ValueError("parser_wrapper required for Python fallback")
    return parser_wrapper.parse(path, is_dependency, index_source=index_source, **kwargs)


def parse_files_parallel(file_specs, num_threads=None):
    """Parse multiple files in parallel using Rust.

    Args:
        file_specs: list of (path_str, lang, is_dependency) tuples
        num_threads: optional thread count (0 = auto)

    Returns:
        list of file_data dicts

    Raises:
        RuntimeError if Rust engine not available
    """
    if not RUST_AVAILABLE:
        raise RuntimeError("Rust engine required for parallel parsing")

    return _rust_parse_files_parallel(file_specs, num_threads)


def pre_scan_for_imports(file_specs):
    """Pre-scan files to build imports_map.

    Args:
        file_specs: list of (path_str, extension) tuples

    Returns:
        dict {symbol_name: [file_path, ...]}

    Raises:
        RuntimeError if Rust engine not available
    """
    if not RUST_AVAILABLE:
        raise RuntimeError("Rust engine required for pre_scan_for_imports")

    return _rust_pre_scan(file_specs)


def resolve_call_groups(all_file_data, imports_map, skip_external=False):
    """Resolve function calls into 6-category groups using Rust."""
    if not RUST_AVAILABLE:
        raise RuntimeError("Rust engine required for resolve_call_groups")
    return _rust_resolve_calls(all_file_data, imports_map, skip_external)


def resolve_inheritance(all_file_data, imports_map):
    """Resolve inheritance links using Rust."""
    if not RUST_AVAILABLE:
        raise RuntimeError("Rust engine required for resolve_inheritance")
    return _rust_resolve_inheritance(all_file_data, imports_map)


def sanitize_props(props):
    """Sanitize properties for graph DB storage using Rust."""
    if not RUST_AVAILABLE:
        raise RuntimeError("Rust engine required for sanitize_props")
    return _rust_sanitize_props(props)


# Languages supported by the Rust engine (Phase 4: 17 languages)
# Perl and Kotlin disabled due to tree-sitter version incompatibility
_RUST_SUPPORTED_LANGS = {
    "python", "javascript", "typescript", "tsx", "go", "java", "cpp",
    "c", "rust", "ruby", "c_sharp", "php",
    "scala", "swift", "haskell", "dart", "elixir",
}
