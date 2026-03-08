# 🐍 Virtual Environment Setup Guide

## Overview

The CodeGraphContext VS Code extension automatically detects and uses `cgc` from Python virtual environments. This guide explains how it works and how to configure it.

## How Auto-Detection Works

The extension searches for `cgc` in the following order:

1. **User-configured path** (if set in `cgc.cgcPath` setting)
2. **Workspace virtual environments**:
   - `.venv/` (recommended)
   - `venv/`
   - `env/`
   - `.env/`
3. **Python extension's selected interpreter**
4. **System PATH** (fallback)

## Recommended Setup

### Option 1: Workspace Virtual Environment (Recommended)

This is the easiest and most reliable method.

#### Step 1: Create Virtual Environment
```bash
# Navigate to your workspace
cd /path/to/your/workspace

# Create virtual environment
python3 -m venv .venv

# Activate it
source .venv/bin/activate  # On Linux/Mac
# or
.venv\Scripts\activate  # On Windows
```

#### Step 2: Install CodeGraphContext
```bash
pip install codegraphcontext
```

#### Step 3: Verify Installation
```bash
which cgc  # Should show .venv/bin/cgc
cgc --version
```

#### Step 4: Open in VS Code
```bash
code .
```

The extension will automatically detect and use `.venv/bin/cgc`!

### Option 2: Use Python Extension's Interpreter

If you're using the Python extension and have selected an interpreter:

#### Step 1: Select Python Interpreter
1. Press `Cmd+Shift+P` (Mac) or `Ctrl+Shift+P` (Windows/Linux)
2. Type "Python: Select Interpreter"
3. Choose your virtual environment

#### Step 2: Install CodeGraphContext
Make sure `cgc` is installed in that environment:
```bash
# Activate the selected environment
source /path/to/venv/bin/activate

# Install cgc
pip install codegraphcontext
```

The extension will detect `cgc` from the Python extension's selected interpreter.

### Option 3: Manual Path Configuration

If auto-detection doesn't work, you can manually specify the path:

#### Step 1: Find cgc Path
```bash
# Activate your virtual environment
source /path/to/venv/bin/activate

# Get the full path to cgc
which cgc
# Example output: /home/user/project/.venv/bin/cgc
```

#### Step 2: Configure in VS Code
1. Press `Cmd+,` (Mac) or `Ctrl+,` (Windows/Linux)
2. Search for "cgc.cgcPath"
3. Enter the full path: `/home/user/project/.venv/bin/cgc`

## Platform-Specific Notes

### Linux/Mac
Virtual environment structure:
```
.venv/
├── bin/
│   ├── python
│   ├── pip
│   └── cgc          ← Extension looks here
└── lib/
```

### Windows
Virtual environment structure:
```
.venv/
├── Scripts/
│   ├── python.exe
│   ├── pip.exe
│   └── cgc.exe      ← Extension looks here
└── Lib/
```

## Troubleshooting

### Error: "spawn cgc ENOENT"

This means the extension can't find the `cgc` executable.

**Solutions:**

1. **Check if cgc is installed:**
   ```bash
   # Activate your venv
   source .venv/bin/activate
   
   # Check if cgc exists
   which cgc
   cgc --version
   ```

2. **Install cgc in your virtual environment:**
   ```bash
   pip install codegraphcontext
   ```

3. **Verify virtual environment location:**
   Make sure your venv is in one of these locations:
   - `.venv/` (recommended)
   - `venv/`
   - `env/`
   - `.env/`

4. **Check VS Code Output:**
   - Open Output panel: View → Output
   - Select "CodeGraphContext" from dropdown
   - Look for messages like "Found cgc in virtual environment: ..."

5. **Set path manually:**
   - Settings → Search "cgc.cgcPath"
   - Set to full path: `/path/to/.venv/bin/cgc`

### Extension Not Finding Virtual Environment

**Check workspace folder:**
```bash
# Make sure you opened the workspace folder, not a parent folder
code /path/to/your/project  # Correct
# Not: code /path/to/parent
```

**Check virtual environment exists:**
```bash
ls -la .venv/bin/cgc  # Should exist
```

**Check Output panel:**
The extension logs which path it's using. Check the Output panel for messages.

### Multiple Virtual Environments

If you have multiple projects with different virtual environments:

**Option A: Use workspace-specific settings**

Create `.vscode/settings.json` in each project:
```json
{
  "cgc.cgcPath": "/path/to/project1/.venv/bin/cgc"
}
```

**Option B: Use relative paths**

The extension automatically searches the current workspace, so just ensure each project has its own `.venv/`.

### Using System-Wide Installation

If you want to use a system-wide `cgc` installation:

```bash
# Install globally (not recommended)
pip install --user codegraphcontext

# Or in a specific location
pip install --target=/opt/cgc codegraphcontext
```

Then set `cgc.cgcPath` to the full path.

## Best Practices

### ✅ Recommended
- Use `.venv/` in your workspace root
- Install cgc in the same venv as your project dependencies
- Let the extension auto-detect the path

### ❌ Not Recommended
- Installing cgc globally (can cause version conflicts)
- Using different Python versions for cgc and your project
- Hardcoding absolute paths (not portable across machines)

## Verification Checklist

Use this checklist to verify your setup:

- [ ] Virtual environment exists in workspace (`.venv/`, `venv/`, etc.)
- [ ] Virtual environment is activated
- [ ] `cgc` is installed: `pip list | grep codegraphcontext`
- [ ] `cgc` executable exists: `which cgc` or `where cgc`
- [ ] `cgc` works: `cgc --version`
- [ ] VS Code is opened at workspace root
- [ ] Extension Output shows correct cgc path
- [ ] Extension commands work (try "CGC: Index Current Workspace")

## Example Setups

### Example 1: Python Project with .venv

```bash
my-project/
├── .venv/              # Virtual environment
│   └── bin/
│       └── cgc         # Auto-detected!
├── src/
│   └── main.py
├── requirements.txt
└── .vscode/
    └── settings.json   # Optional overrides
```

**No configuration needed!** Extension auto-detects `.venv/bin/cgc`.

### Example 2: Monorepo with Multiple Projects

```bash
monorepo/
├── project1/
│   ├── .venv/
│   │   └── bin/cgc
│   └── src/
├── project2/
│   ├── .venv/
│   │   └── bin/cgc
│   └── src/
└── .vscode/
    └── settings.json
```

**Open each project separately:**
```bash
code monorepo/project1  # Uses project1/.venv/bin/cgc
code monorepo/project2  # Uses project2/.venv/bin/cgc
```

### Example 3: Shared Virtual Environment

```bash
workspace/
├── shared-venv/
│   └── bin/
│       └── cgc
├── project1/
└── project2/
```

**Configure manually:**

`.vscode/settings.json`:
```json
{
  "cgc.cgcPath": "${workspaceFolder}/shared-venv/bin/cgc"
}
```

## Advanced Configuration

### Using Environment Variables

You can use environment variables in the path:

```json
{
  "cgc.cgcPath": "${env:HOME}/.local/bin/cgc"
}
```

### Workspace-Specific Settings

Create `.vscode/settings.json` in your workspace:

```json
{
  "cgc.cgcPath": "${workspaceFolder}/.venv/bin/cgc",
  "cgc.autoIndex": true,
  "cgc.databasePath": "${workspaceFolder}/.cgc/db"
}
```

### Using Different Database Backends

If your virtual environment has a specific database backend:

```json
{
  "cgc.databaseType": "falkordb",  // or "neo4j"
  "cgc.databasePath": "${workspaceFolder}/.cgc/falkordb"
}
```

## Docker/Container Environments

If you're using Docker or containers:

### Option 1: Install Extension in Container

Use VS Code Remote - Containers extension:
1. Install Remote - Containers extension
2. Open workspace in container
3. Install CGC extension in container
4. cgc will be available in container's PATH

### Option 2: Use Remote Path

Configure the path to point to the container:
```json
{
  "cgc.cgcPath": "/usr/local/bin/cgc"
}
```

## Summary

The extension makes it easy to use `cgc` from virtual environments:

1. **Create a virtual environment** in your workspace (`.venv/` recommended)
2. **Install cgc**: `pip install codegraphcontext`
3. **Open workspace in VS Code**: The extension auto-detects cgc!

If auto-detection doesn't work, manually set `cgc.cgcPath` in settings.

---

**Need help?** Check the [main README](README.md) or open an issue on [GitHub](https://github.com/CodeGraphContext/CodeGraphContext/issues).
