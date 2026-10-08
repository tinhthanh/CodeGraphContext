"""Entry point for the `cgc` / `codegraphcontext` commands.

The original CodeGraphContext CLI (typer, graph-DB backends, MCP server) is an
optional extra; give a clear hint instead of a bare ModuleNotFoundError when it
isn't installed. `cgc-wiki` works with the core install.
"""

import sys


def main() -> None:
    try:
        from codegraphcontext.cli.main import app
    except ModuleNotFoundError as exc:
        sys.stderr.write(
            f"The `cgc` CLI needs optional dependencies ({exc.name} is missing).\n"
            'Install them with:  pip install "codegraphcontext-rust[full]"\n'
            "Code indexing for wiki-forge only needs `cgc-wiki`, which works without them.\n"
        )
        sys.exit(1)
    app()
