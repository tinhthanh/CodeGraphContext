"""Extract design rationale from source code comments.

Captures: NOTE, WHY, HACK, TODO, FIXME, IMPORTANT, WARNING, SAFETY,
WORKAROUND, ASSUMPTION, CONSTRAINT, DECISION, TRADE-OFF comments.

These explain "why" code was written a certain way — the most valuable
information for documentation that pure AST parsing misses.
"""

from __future__ import annotations

import logging
import os
import re
from pathlib import Path
from typing import Any, Dict, List

logger = logging.getLogger(__name__)

# Patterns that indicate design rationale in comments
_RATIONALE_PATTERN = re.compile(
    r'(?:^|\s)(?://|#|/\*\*?|\*)\s*'  # comment prefix: //, #, /*, *, /**
    r'(NOTE|WHY|HACK|TODO|FIXME|IMPORTANT|WARNING|SAFETY|'
    r'WORKAROUND|ASSUMPTION|CONSTRAINT|DECISION|TRADE.?OFF|'
    r'REVIEW|OPTIMIZE|DEPRECATED|BREAKING|MIGRATION|'
    r'CONTEXT|REASON|RATIONALE|EXPLAIN|CAVEAT|GOTCHA|PITFALL)'
    r'\s*:?\s*(.+)',
    re.IGNORECASE | re.MULTILINE,
)

# Language extensions that support comments
_COMMENT_LANGS = {
    '.py', '.js', '.jsx', '.ts', '.tsx', '.java', '.go', '.rs',
    '.rb', '.php', '.cs', '.kt', '.scala', '.swift', '.c', '.cpp',
    '.h', '.hpp', '.m', '.dart', '.ex', '.exs', '.hs', '.pl',
}


def extract_rationales(
    parsed_results: List[Dict[str, Any]],
    repo_path: str,
) -> List[Dict[str, Any]]:
    """Extract design rationale comments from source files.

    Returns list of:
        {
            tag: str (NOTE, WHY, HACK, TODO, etc.),
            text: str (the rationale text),
            file: str (relative path),
            line: int,
            context: str (nearby function/class name),
        }
    """
    rationales: List[Dict[str, Any]] = []

    for file_data in parsed_results:
        if "error" in file_data:
            continue

        file_path = file_data.get("path", "")
        ext = Path(file_path).suffix.lower()

        if ext not in _COMMENT_LANGS:
            continue

        if not os.path.exists(file_path):
            continue

        try:
            rel_path = os.path.relpath(file_path, repo_path)
        except ValueError:
            rel_path = Path(file_path).name

        # Build line → function context map
        func_context: Dict[int, str] = {}
        for fn in file_data.get("functions", []):
            start = fn.get("line_number", 0)
            end = fn.get("body_end_line", start + 50) or start + 50
            name = fn.get("name", "")
            ctx = fn.get("class_context", "")
            full_name = f"{ctx}.{name}" if ctx else name
            for line in range(start, end + 1):
                func_context[line] = full_name

        # Scan source for rationale comments
        try:
            with open(file_path, "r", encoding="utf-8", errors="replace") as fh:
                for line_no, line in enumerate(fh, 1):
                    for m in _RATIONALE_PATTERN.finditer(line):
                        tag = m.group(1).upper()
                        text = m.group(2).strip()

                        # Decode unicode escapes (e.g., \u0111\u00e3 → đã)
                        try:
                            text = text.encode("utf-8").decode("unicode_escape").encode("latin-1").decode("utf-8")
                        except (UnicodeDecodeError, UnicodeEncodeError):
                            pass  # keep original if decode fails

                        # Skip very short or empty rationales
                        if len(text) < 5:
                            continue

                        # Find nearest function context
                        context = func_context.get(line_no, "")
                        if not context:
                            # Check lines above
                            for delta in range(1, 10):
                                context = func_context.get(line_no - delta, "")
                                if context:
                                    break

                        rationales.append({
                            "tag": tag,
                            "text": text,
                            "file": rel_path,
                            "line": line_no,
                            "context": context,
                        })
        except OSError:
            continue

    # Sort by file + line
    rationales.sort(key=lambda r: (r["file"], r["line"]))
    logger.info("Extracted %d rationale comments", len(rationales))
    return rationales
