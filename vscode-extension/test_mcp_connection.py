
import subprocess
import json
import os
import sys
import time
import threading

def read_output(process):
    while True:
        line = process.stdout.readline()
        if not line:
            break
        print(f"STDOUT: {line.decode('utf-8').strip()}")

def read_error(process):
    while True:
        line = process.stderr.readline()
        if not line:
            break
        print(f"STDERR: {line.decode('utf-8').strip()}")

def test_mcp():
    # Detect cgc path the same way the extension does broadly
    # defaulting to 'cgc'
    cgc_path = 'cgc'
    
    # Try to find it in the user's venv if possible to match their env
    venv_cgc = '/home/shashank/Desktop/CodeGraphContext/.venv/bin/cgc'
    if os.path.exists(venv_cgc):
        cgc_path = venv_cgc
    
    print(f"Using cgc executable: {cgc_path}")

    env = os.environ.copy()
    env['PYTHONUNBUFFERED'] = '1'

    process = subprocess.Popen(
        [cgc_path, 'mcp', 'start'],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        bufsize=0 # Unbuffered
    )

    # Start threads to read output
    t_err = threading.Thread(target=read_error, args=(process,))
    t_err.daemon = True
    t_err.start()

    print("Process started, sending initialize request...")
    
    # JSON-RPC Initialize Request
    init_req = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "clientInfo": {"name": "test-script", "version": "1.0"}
        }
    }

    # Write to stdin
    message = json.dumps(init_req)
    # MCP (and JSON-RPC often) might assume Content-Length header if using VSCode JSONRPC libraries,
    # OR it might just be newline delimited JSON.
    # VSCode's StreamMessageWriter uses Content-Length headers by default unless configured otherwise.
    # Standard MCP usually uses JSON-RPC over stdio which is often line-delimited or header-based.
    # Let's try sending just the JSON line first (LSP style usually needs headers).
    
    # Based on the extension code: 
    # new rpc.StreamMessageReader(this.process.stdout!),
    # new rpc.StreamMessageWriter(this.process.stdin!)
    # vscode-jsonrpc StreamMessageWriter sends Content-Length headers by default.
    
    content = message.encode('utf-8')
    header = f"Content-Length: {len(content)}\r\n\r\n".encode('utf-8')
    
    print(f"Sending: {header + content}")
    
    process.stdin.write(header + content)
    process.stdin.flush()
    
    # Also try sending just plain newline terminated JSON in case it's simple stdio
    # process.stdin.write(content + b'\n')
    # process.stdin.flush()

    print("Waiting for response...")
    
    # Read stdout manually to see what we get
    start = time.time()
    while time.time() - start < 5:
        line = process.stdout.readline()
        if line:
            print(f"RESPONSE STDOUT: {line.decode('utf-8').strip()}")
        time.sleep(0.1)

    process.kill()

if __name__ == "__main__":
    test_mcp()
