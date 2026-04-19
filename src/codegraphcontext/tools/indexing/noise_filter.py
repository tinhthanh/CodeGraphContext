"""Shared god-node noise filter for graph reports and visualization.

Used by both CGC (cgc_wiki_cli.py) and llm-wiki-v3 (cgc_bridge.py)
to filter out accessors, test DSL, stdlib, and other noisy function
names from top-connected / god-node reports.
"""
from __future__ import annotations

import re

# Function names that are noise in call graphs (not real architectural hubs)
GOD_NODE_NOISE = frozenset({
    # Java/generic accessors
    "get", "set", "of", "put", "add", "remove", "size", "isEmpty",
    "equals", "hashCode", "toString", "compareTo", "valueOf", "values",
    "build", "builder", "clone", "close", "read", "write",
    "from", "apply", "accept", "test", "run", "call",
    # Lombok-style getters/setters
    "getId", "getName", "getStatus", "getType", "getValue", "getCode",
    "getCreatedAt", "getUpdatedAt", "getPath", "getMessage",
    "setId", "setName", "setStatus", "setType", "setValue",
    # Jackson / JSON
    "asText", "asLong", "asInt", "asBoolean", "isMissingNode", "isArray",
    "path", "jsonPath", "readTree", "readValue", "writeValueAsString",
    # Test DSL (JUnit, MockMvc, Jest, Vitest, Testing Library)
    "it", "describe", "expect", "toBe", "toEqual", "assert",
    "andExpect", "assertThat", "verify", "mock", "when", "given",
    "mockResolvedValue", "mockReturnValue", "waitForTimeout",
    "perform", "isOk", "content", "header", "value",
    "toHaveBeenCalled", "toHaveBeenCalledWith", "toContain",
    "toBeTruthy", "toBeFalsy", "toBeDefined", "toBeNull",
    "render", "screen", "fireEvent", "waitFor", "userEvent",
    "vi", "beforeEach", "afterEach", "beforeAll", "afterAll",
    # HTTP / response
    "status", "ok", "body", "json", "parse", "stringify",
    # Logging
    "log", "error", "warn", "info", "debug", "println", "printf",
    # Java stdlib
    "format", "trim", "split", "join", "replace", "contains",
    "append", "insert", "delete", "substring", "length",
    "stream", "map", "filter", "collect", "forEach", "reduce",
    "Optional", "orElse", "orElseThrow", "isPresent", "ifPresent",
    "currentTimeMillis", "nanoTime", "now",
    # TS/JS stdlib built-ins
    "String", "Date", "Number", "Boolean", "Array", "Object",
    "Error", "Float32Array", "Int32Array", "Uint8Array", "Math",
    "Promise", "Symbol", "Set", "Map", "JSON",
    # Tailwind / className utilities
    "cn", "clsx", "cx", "tw", "twMerge",
    # React hooks (too noisy to count as hub)
    "useState", "useEffect", "useCallback", "useMemo", "useRef",
    "useContext", "useReducer", "useLayoutEffect",
    # Python stdlib + test DSL
    "uuid4", "uuid1", "print", "len", "str", "int", "float", "dict",
    "list", "tuple", "range", "isinstance", "hasattr", "getattr",
    "setattr", "type", "enumerate", "zip", "sorted", "reversed",
    "MagicMock", "AsyncMock", "Mock", "patch",
    "pytest", "fixture", "raises", "mark",
    # SQLAlchemy declarative noise
    "Column", "String", "Integer", "Text", "Boolean", "DateTime",
    "ForeignKey", "Index", "UniqueConstraint", "Table",
    # Go testing + testify + gomock + stdlib
    "T", "B", "M", "Helper", "Logf", "Skip", "SkipNow", "FailNow",
    "Errorf", "Fatalf", "Error", "Fatal", "Log",
    "NoError", "Equal", "NotEqual", "Nil", "NotNil", "True", "False",
    "Empty", "NotEmpty", "Len", "Contains", "NotContains",
    "Greater", "Less", "InDelta", "ElementsMatch", "Subset",
    "EXPECT", "DoAndReturn", "Return", "Times", "AnyTimes", "Do",
    "Finish", "NewController", "Call",
    "Background", "TODO", "WithValue", "WithCancel", "WithTimeout",
    "Printf", "Println", "Sprintf", "Sprintln",
    "Make", "Append", "Copy", "Close", "Open", "Read", "Write",
    # Go logger levels + HTTP response primitives
    "Info", "Warn", "Debug", "Trace",
    "WriteString", "SendString", "WriteJSON", "SendStatus",
    "Next", "Locals", "Param", "Params", "Query", "QueryParser",
    "BodyParser", "ParamsParser",
    # Go testing extra
    "Parallel", "Cleanup", "Deadline",
    # Angular reactive forms
    "FormControl", "FormGroup", "FormArray", "FormBuilder", "Validators",
    "AbstractControl", "ControlValueAccessor",
    # Angular testing
    "TestBed", "inject", "fakeAsync", "tick", "flush", "waitForAsync",
    "ComponentFixture", "DebugElement",
    # RxJS
    "Subject", "BehaviorSubject", "ReplaySubject", "Observable",
    "of", "from", "pipe", "tap", "switchMap",
    "mergeMap", "catchError", "take", "takeUntil", "subscribe",
    # Ionic
    "ModalController", "ToastController", "AlertController",
    "LoadingController", "NavController",
})

# Regex patterns for accessor detection
ACCESSOR_RE = re.compile(r"^(get|set|is|has)[A-Z]")
RECORD_ACCESSOR_RE = re.compile(r"^[a-z][a-zA-Z]*$")
REACT_HOOK_RE = re.compile(r"^use[A-Z]\w*$")

# Functions that look like record accessors but are meaningful
MEANINGFUL_NAMES = frozenset({
    "main", "run", "start", "execute", "init", "setup", "configure",
    "handle", "process", "validate", "create", "update", "delete",
    "save", "load", "find", "search", "index", "render", "dispatch",
    "schedule", "publish", "subscribe", "connect", "disconnect",
    "authenticate", "authorize", "transform", "convert", "migrate",
    "fetch", "post", "get", "put", "patch",
})


def is_noise_node(name: str, call_count: int = 0) -> bool:
    """Return True if this function name is noise (not a real architectural hub).

    Args:
        name: Function/class name.
        call_count: Number of times this function is called (for record accessor heuristic).
    """
    if name in GOD_NODE_NOISE:
        return True
    # React hooks are meaningful — keep
    if REACT_HOOK_RE.match(name):
        return False
    if ACCESSOR_RE.match(name):
        return True
    # Record accessor heuristic: short camelCase name, not meaningful, high call count
    if (RECORD_ACCESSOR_RE.match(name)
            and name not in MEANINGFUL_NAMES
            and len(name) <= 20
            and call_count >= 5):
        return True
    return False
