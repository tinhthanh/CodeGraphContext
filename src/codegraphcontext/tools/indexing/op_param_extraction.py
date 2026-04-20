"""Extract operational parameters from source code.

Captures: timeouts, cron schedules, retry configs, pool sizes,
rate limits, and other runtime configuration values.

These are found in:
- Variable declarations with config-like names
- Annotation arguments (@Scheduled, @Retryable, @Backoff)
- Method calls (setTimeout, setConnectTimeout, etc.)
- Constants (MAX_*, TIMEOUT_*, etc.)
"""

from __future__ import annotations

import logging
import os
import re
from pathlib import Path
from typing import Any, Dict, List

logger = logging.getLogger(__name__)

# Regex for config-like variable/constant names
_NAME_PATTERN = re.compile(
    r"(?i)(timeout|cron|schedule|interval|retry|retries|"
    r"max_|min_|port|host|redis|celery|ttl|cooldown|"
    r"batch|concurren|threshold|limit|delay|period|frequency|"
    r"worker|queue|pool.?size|backoff|rate.?limit|"
    r"expir|duration|capacity|buffer.?size|chunk)"
)

# Patterns to extract from source lines
_LINE_PATTERNS = [
    # Java annotations: @Scheduled(fixedRate = 21600000)
    re.compile(
        r"@(?:Scheduled|Retryable|Backoff|CacheConfig|Cacheable)\s*\("
        r"([^)]+)\)",
        re.IGNORECASE,
    ),
    # Java/general: setTimeout(30000), setConnectTimeout(30000)
    re.compile(
        r"(?:set|get)?(?:Connect|Read|Write|Socket|Request)?Timeout\s*\(\s*(\d+)",
        re.IGNORECASE,
    ),
    # Constant assignments: MAX_RETRIES = 3, TIMEOUT_MS = 5000
    re.compile(
        r"(?:final\s+)?(?:static\s+)?(?:final\s+)?(?:\w+\s+)?"
        r"([A-Z][A-Z_0-9]*(?:TIMEOUT|RETRY|LIMIT|INTERVAL|DELAY|MAX|MIN|TTL|CRON|POOL|BATCH|RATE|BUFFER|DURATION|CAPACITY)[\w]*)"
        r"\s*=\s*([^;{]+)",
        re.MULTILINE,
    ),
    # Python/YAML-style: timeout = 30, max_retries = 3
    re.compile(
        r"^\s*([a-z][a-z_0-9]*(?:timeout|retry|limit|interval|delay|max|min|ttl|cron|pool|batch|rate|buffer|duration|capacity)[\w]*)"
        r"\s*[:=]\s*(.+?)(?:\s*#.*)?$",
        re.IGNORECASE | re.MULTILINE,
    ),
]

# Languages to scan
_SCAN_EXTENSIONS = {
    ".java", ".py", ".js", ".ts", ".go", ".rs", ".rb", ".php",
    ".kt", ".scala", ".swift", ".cs", ".c", ".cpp",
}

# Value validation: only keep values that look like actual config values
# (numbers, string literals, durations, cron expressions, booleans)
# Reject: type declarations (String, Integer, CompletableFuture<T>, etc.)
_VALID_VALUE_RE = re.compile(
    r"^("
    r"\d[\d_.,]*"                      # numbers: 30000, 5_000, 3.14
    r"|['\"].*['\"]"                   # string literals: "0 */6 * * *"
    r"|true|false|True|False"          # booleans
    r"|null|None|nil"                  # null values
    r"|\d+\s*\*\s*\d+"                 # expressions: 60 * 1000
    r"|Duration\.of\w+"                # Java Duration.ofSeconds(30)
    r"|Period\.of\w+"                  # Java Period.ofDays(7)
    r"|TimeUnit\.\w+"                  # Java TimeUnit.SECONDS
    r"|timedelta\("                    # Python timedelta(...)
    r"|\d+[smhd]"                      # duration shorthand: 30s, 5m, 1h, 7d
    r")"
)

# Type names that indicate a type declaration (NOT a config value)
_TYPE_DECLARATION_RE = re.compile(
    r"^(String|Integer|Long|Double|Float|Boolean|Byte|Short|Character"
    r"|int|long|double|float|boolean|byte|short|char|void"
    r"|List|Map|Set|Queue|Deque|Collection|Optional"
    r"|CompletableFuture|Future|Callable|Runnable|Supplier|Consumer"
    r"|AtomicInteger|AtomicLong|AtomicReference|AtomicBoolean"
    r"|BigDecimal|BigInteger"
    r"|LocalDateTime|Instant|Date|Duration|ZonedDateTime"
    r"|[A-Z]\w*<)"                     # any Generic<T>
)


def _is_valid_config_value(value: str) -> bool:
    """Return True if value looks like an actual config value, not a type declaration."""
    v = value.strip().rstrip(",;")
    if not v:
        return False
    # Accept known config value patterns FIRST (before type check)
    if _VALID_VALUE_RE.match(v):
        return True
    # Reject type declarations
    if _TYPE_DECLARATION_RE.match(v):
        return False
    # Accept if it's a short constant reference (all caps, e.g. MAX_RETRIES)
    if re.match(r"^[A-Z][A-Z_0-9]+$", v):
        return True
    # Accept annotation values (already validated by annotation pattern)
    if v.startswith("@"):
        return True
    # Reject anything that looks like a type (starts with uppercase, contains generic)
    if re.match(r"^[A-Z][a-zA-Z]*$", v) or "<" in v:
        return False
    # Accept numeric expressions
    if re.match(r"^[\d\s+\-*/().]+$", v):
        return True
    return False


def extract_operational_params(
    parsed_results: List[Dict[str, Any]],
    repo_path: str,
) -> List[Dict[str, Any]]:
    """Extract operational parameters from source files.

    Returns list of:
        {
            name: str,
            value: str,
            path: str (relative),
            line_number: int,
            category: str (timeout|schedule|retry|limit|pool|config),
        }
    """
    params: List[Dict[str, Any]] = []
    seen: set = set()  # deduplicate

    # Pre-filter: only scan files likely to have config values
    # (files with config-like variable names, or Java/Python config files)
    _CONFIG_PATH_HINTS = {"config", "setting", "constant", "env", "properties"}
    files_to_scan = []
    for file_data in parsed_results:
        if "error" in file_data:
            continue
        file_path = file_data.get("path", "")
        ext = Path(file_path).suffix.lower()
        if ext not in _SCAN_EXTENSIONS or not os.path.exists(file_path):
            continue
        # Scan if: has config-like variables, or path contains config hints
        has_config_var = any(
            _NAME_PATTERN.search(v.get("name", ""))
            for v in file_data.get("variables", [])
        )
        path_lower = file_path.lower()
        has_config_path = any(h in path_lower for h in _CONFIG_PATH_HINTS)
        # Java/Python files with decorators (@Scheduled, @Retryable)
        has_decorators = any(
            any(d for d in f.get("decorators", []) or []
                if any(k in str(d).lower() for k in ("scheduled", "retryable", "backoff", "cacheable")))
            for f in file_data.get("functions", [])
        )
        if has_config_var or has_config_path or has_decorators:
            files_to_scan.append(file_data)

    logger.debug("op_params: scanning %d/%d files (pre-filtered)", len(files_to_scan), len(parsed_results))

    for file_data in files_to_scan:
        file_path = file_data.get("path", "")
        ext = Path(file_path).suffix.lower()

        try:
            rel_path = os.path.relpath(file_path, repo_path)
        except ValueError:
            rel_path = Path(file_path).name

        try:
            with open(file_path, "r", encoding="utf-8", errors="replace") as fh:
                for line_no, line in enumerate(fh, 1):
                    # Skip comments-only lines and imports
                    stripped = line.strip()
                    if stripped.startswith(("//", "#", "*", "/*", "import ", "package ")):
                        continue

                    for pattern in _LINE_PATTERNS:
                        for m in pattern.finditer(line):
                            groups = m.groups()
                            if len(groups) >= 2:
                                name = groups[0].strip()
                                value = groups[1].strip().rstrip(",;")
                            elif len(groups) == 1:
                                # Annotation args: parse key=value pairs
                                arg_str = groups[0]
                                for kv in re.finditer(r"(\w+)\s*=\s*([^,)]+)", arg_str):
                                    k, v = kv.group(1).strip(), kv.group(2).strip()
                                    key = f"@{m.group(0).split('(')[0].strip('@')}({k})"
                                    dedup_key = f"{key}|{rel_path}|{line_no}"
                                    if dedup_key in seen:
                                        continue
                                    seen.add(dedup_key)
                                    params.append({
                                        "name": key,
                                        "value": v,
                                        "path": rel_path,
                                        "line_number": line_no,
                                        "category": _categorize(key),
                                    })
                                continue
                            else:
                                continue

                            # Check if name matches config pattern
                            if not _NAME_PATTERN.search(name):
                                continue

                            # Validate value is a real config value, not a type declaration
                            if not _is_valid_config_value(value):
                                continue

                            dedup_key = f"{name}|{rel_path}|{line_no}"
                            if dedup_key in seen:
                                continue
                            seen.add(dedup_key)

                            params.append({
                                "name": name,
                                "value": value[:200],
                                "path": rel_path,
                                "line_number": line_no,
                                "category": _categorize(name),
                            })
        except OSError:
            continue

    # Also scan variables from parsed results
    for file_data in parsed_results:
        if "error" in file_data:
            continue
        file_path = file_data.get("path", "")
        try:
            rel_path = os.path.relpath(file_path, repo_path)
        except ValueError:
            rel_path = Path(file_path).name

        for var in file_data.get("variables", []):
            name = var.get("name", "")
            if not _NAME_PATTERN.search(name):
                continue
            var_value = var.get("type", "")
            # Only keep variables whose captured value looks like a real config
            # value. Empty/missing values (common from parsers that extract
            # declarations but not literals) are noise in a config panel.
            if not var_value or not _is_valid_config_value(var_value):
                continue
            dedup_key = f"{name}|{rel_path}|{var.get('line_number', 0)}"
            if dedup_key in seen:
                continue
            seen.add(dedup_key)
            params.append({
                "name": name,
                "value": var_value,
                "path": rel_path,
                "line_number": var.get("line_number", 0),
                "category": _categorize(name),
            })

    params.sort(key=lambda p: (p["path"], p["line_number"]))
    logger.info("Extracted %d operational parameters", len(params))
    return params


def _categorize(name: str) -> str:
    """Categorize an operational parameter by name."""
    n = name.lower()
    if any(x in n for x in ("timeout", "ttl", "expir", "duration")):
        return "timeout"
    if any(x in n for x in ("cron", "schedule", "interval", "period", "frequency")):
        return "schedule"
    if any(x in n for x in ("retry", "retries", "backoff")):
        return "retry"
    if any(x in n for x in ("max", "min", "limit", "threshold", "capacity")):
        return "limit"
    if any(x in n for x in ("pool", "worker", "queue", "concurren", "batch")):
        return "pool"
    return "config"
