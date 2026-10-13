"""discover_files must be deterministic: the resolver breaks ties by input order."""
from codegraphcontext.tools.indexing.cgc_wiki_cli import discover_files


def test_discover_files_is_sorted_and_filtered(tmp_path):
    for rel in ["b/z.py", "a/y.ts", "a/x.py", "c/w.java", "node_modules/pkg/i.js", "README.md"]:
        p = tmp_path / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("x = 1\n")

    files = discover_files(str(tmp_path))
    rels = [str(p)[len(str(tmp_path.resolve())) + 1:] for p, _ in files]
    assert rels == ["a/x.py", "a/y.ts", "b/z.py", "c/w.java"]
    assert files == sorted(files)
    assert discover_files(str(tmp_path)) == files
