# 🔍 Debugging Guide - Why Functions/Classes Don't Show

## Quick Diagnostic Steps

### Step 1: Check VS Code Output Panel

1. Open Output panel: `View → Output` (or `Cmd+Shift+U`)
2. Select "CodeGraphContext" from the dropdown
3. Look for error messages

### Step 2: Check Developer Console

1. Open Developer Tools: `Help → Toggle Developer Tools`
2. Go to "Console" tab
3. Look for red error messages
4. Share any errors you see

### Step 3: Test Query Manually

Run this in terminal to verify the query works:

```bash
cd /home/shashank/Desktop/CodeGraphContext
.venv/bin/cgc query "MATCH (file:File)-[:CONTAINS]->(f:Function) RETURN f.name as name, file.path as file, f.start_line as line LIMIT 10"
```

Should return JSON with functions.

### Step 4: Check Extension is Latest Version

```bash
# Check installed version timestamp
ls -lah ~/.vscode/extensions/codegraphcontext.codegraphcontext-0.1.0/

# Should show Feb 4 22:30 or later
```

### Step 5: Force Reload Extension

1. Press `Cmd+Shift+P`
2. Type "Developer: Reload Window"
3. Press Enter
4. Wait for extension to activate

### Step 6: Manually Trigger Refresh

1. Click on "FUNCTIONS" panel in the sidebar
2. Click the refresh icon (↻) in the panel title bar
3. Wait a few seconds

## Common Issues

### Issue 1: Extension Not Activated

**Symptom**: Panels are empty, no errors

**Solution**:
1. Check if CGC icon appears in Activity Bar (left sidebar)
2. If not, extension didn't activate
3. Reload window: `Cmd+Shift+P` → "Developer: Reload Window"

### Issue 2: Wrong Workspace

**Symptom**: "No projects indexed yet"

**Solution**:
Make sure you opened the correct workspace:
```bash
code /home/shashank/Desktop/CodeGraphContext
```

Not the vscode-extension folder!

### Issue 3: Database Not Indexed

**Symptom**: `cgc list` shows projects but panels are empty

**Solution**:
```bash
# Re-index
.venv/bin/cgc index /home/shashank/Desktop/CodeGraphContext --force

# Verify
.venv/bin/cgc list
```

### Issue 4: Old Extension Cached

**Symptom**: Extension installed but still buggy

**Solution**:
```bash
# Completely remove extension
rm -rf ~/.vscode/extensions/codegraphcontext.codegraphcontext-*

# Reinstall
code --install-extension /home/shashank/Desktop/CodeGraphContext/vscode-extension/codegraphcontext-0.1.0.vsix

# Reload
# Cmd+Shift+P → "Developer: Reload Window"
```

### Issue 5: cgc Command Not Found

**Symptom**: Error about cgc not found

**Solution**:
The extension should auto-detect `.venv/bin/cgc`. If not:
1. Open Settings: `Cmd+,`
2. Search "cgc.cgcPath"
3. Set to: `/home/shashank/Desktop/CodeGraphContext/.venv/bin/cgc`
4. Reload window

## What to Share for Debugging

If still not working, share:

1. **Output Panel** content:
   - View → Output → Select "CodeGraphContext"
   - Copy all content

2. **Developer Console** errors:
   - Help → Toggle Developer Tools → Console tab
   - Copy any red errors

3. **Extension Log**:
   - View → Output → Select "Log (Extension Host)"
   - Search for "CodeGraphContext"
   - Copy relevant lines

4. **Test Query Result**:
   ```bash
   .venv/bin/cgc query "MATCH (file:File)-[:CONTAINS]->(f:Function) RETURN f.name as name, file.path as file, f.start_line as line LIMIT 5"
   ```
   Copy the output

5. **Screenshot** of:
   - VS Code with empty panels
   - Developer Tools console

## Expected Behavior

After fixing, you should see:

- **PROJECTS**: "CodeGraphContext" (you already see this! ✅)
- **FUNCTIONS**: List of ~3,637 functions
- **CLASSES**: List of ~776 classes
- **CALL GRAPH**: Shows callers/callees when you select a function
- **DEPENDENCIES**: Shows imports when you open a file

---

**Most Likely Issue**: Extension needs to be completely reloaded or reinstalled.

Try this:
```bash
# 1. Kill VS Code completely
pkill -9 code

# 2. Remove extension
rm -rf ~/.vscode/extensions/codegraphcontext.codegraphcontext-*

# 3. Reinstall
code --install-extension /home/shashank/Desktop/CodeGraphContext/vscode-extension/codegraphcontext-0.1.0.vsix

# 4. Open workspace
code /home/shashank/Desktop/CodeGraphContext

# 5. Wait for extension to activate (check bottom status bar)
```
