"""CSV staging (pyarrow-free bulk load) must round-trip values exactly."""

import duckdb

from codegraphcontext.tools.indexing.persistence.duckdb_writer import (
    _copy_csv,
    _insert_rows,
    _write_csv,
)

TRICKY = [
    "",                                  # empty string, not NULL
    None,                                # NULL
    "007",                               # must not become a number
    'say "hi", then go',                 # quotes + delimiter
    "line1\nline2\r\nline3\rend",        # all newline styles
    "Tiếng Việt — ✓ 🚀",                 # unicode
    "\\N",                               # looks like a NULL marker elsewhere
    "NULL",
    "  padded  ",
    "a\tb",
    "'''single'''",
]


def _roundtrip(tmp_path, values, sql_type):
    c = duckdb.connect()
    c.execute(f"CREATE TABLE t (id INTEGER, v {sql_type})")
    path = str(tmp_path / "t.csv")
    _write_csv({"id": list(range(len(values))), "v": values}, path)
    _copy_csv(c, "t", path)
    return [r[0] for r in c.execute("SELECT v FROM t ORDER BY id").fetchall()]


def test_strings_round_trip_exactly(tmp_path):
    assert _roundtrip(tmp_path, TRICKY, "VARCHAR") == TRICKY


def test_typed_columns(tmp_path):
    assert _roundtrip(tmp_path, [True, False, None], "BOOLEAN") == [True, False, None]
    assert _roundtrip(tmp_path, [0, -5, 2**31 - 1, None], "INTEGER") == [0, -5, 2**31 - 1, None]
    assert _roundtrip(tmp_path, [1.5, 0.0, None], "DOUBLE") == [1.5, 0.0, None]


def test_insert_rows_matches_executemany():
    rows = [(i, v, i % 2 == 0) for i, v in enumerate(TRICKY)]
    a, b = duckdb.connect(), duckdb.connect()
    for c in (a, b):
        c.execute("CREATE TABLE t (id INTEGER, v VARCHAR, flag BOOLEAN, extra VARCHAR DEFAULT 'x')")
    a.executemany("INSERT INTO t (id, v, flag) VALUES (?, ?, ?)", rows)
    _insert_rows(b, "t", rows)  # fewer values than columns → defaults apply
    q = "SELECT * FROM t ORDER BY id"
    assert a.execute(q).fetchall() == b.execute(q).fetchall()


def test_insert_rows_same_with_and_without_pyarrow(monkeypatch):
    import pytest
    from codegraphcontext.tools.indexing.persistence import duckdb_writer as w

    pytest.importorskip("pyarrow")
    rows = [(i, v, i % 2 == 0) for i, v in enumerate(TRICKY)]
    out = []
    for pa in (w._pa, None):
        monkeypatch.setattr(w, "_pa", pa)
        c = duckdb.connect()
        c.execute("CREATE TABLE t (id INTEGER, v VARCHAR, flag BOOLEAN)")
        _insert_rows(c, "t", rows)
        out.append(c.execute("SELECT * FROM t ORDER BY id").fetchall())
    assert out[0] == out[1]
