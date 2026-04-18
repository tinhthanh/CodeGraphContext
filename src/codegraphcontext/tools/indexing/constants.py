"""Shared constants for the indexing pipeline."""

DEFAULT_IGNORE_PATTERNS = [
    # OS metadata
    "__MACOSX/",
    ".DS_Store",
    # Dependency / build directories
    "node_modules/",
    "venv/",
    ".venv/",
    "env/",
    ".env/",
    "dist/",
    "build/",
    "target/",
    "out/",
    ".git/",
    "__pycache__/",
    # Vendor / bundled code (not project source)
    "vendor/",
    ".next/",
    "bower_components/",
    # Binary / media assets
    "*.png",
    "*.jpg",
    "*.jpeg",
    "*.gif",
    "*.svg",
    "*.mp4",
    "*.mp3",
    "*.zip",
    "*.tar",
    "*.gz",
    "*.woff",
    "*.woff2",
    "*.ttf",
    "*.eot",
    "*.ico",
    "*.pdf",
    # Minified / bundled JS/CSS (noise, not project source)
    "*.min.js",
    "*.min.css",
    "*.bundle.js",
    "*.chunk.js",
    # Source maps
    "*.map",
    # Lock files
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "Cargo.lock",
    "poetry.lock",
    "Pipfile.lock",
    "composer.lock",
    "Gemfile.lock",
    "go.sum",
]
