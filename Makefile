# CodeGraphContext — Development Makefile
VENV := .venv
PYTHON := $(VENV)/bin/python
PIP := $(VENV)/bin/pip

.PHONY: setup build test clean

## Setup: create venv + install Python deps + build Rust extension
setup: $(VENV)/bin/activate
	$(PIP) install -e ".[dev]"
	$(PIP) install maturin
	$(MAKE) build

$(VENV)/bin/activate:
	python3 -m venv $(VENV)
	$(PIP) install --upgrade pip

## Build: compile Rust extension into the venv
build:
	. $(VENV)/bin/activate && maturin develop --release

## Test: run Rust unit tests + Python tests
test:
	cd rust && cargo test -p cgc-core
	$(PYTHON) -m pytest tests/ -x -q

## Clean: remove build artifacts
clean:
	rm -rf $(VENV) rust/target
	find . -type d -name __pycache__ -exec rm -rf {} + 2>/dev/null || true
