import React, { useState, useRef, useMemo, useEffect } from 'react';
import { processFiles } from './core/astWorker';
import type { FileData, GraphData } from './core/astWorker';
import { GraphCanvas } from './components/GraphCanvas';
import { UploadCloud, Github, Box, Search, Settings, HelpCircle, Heart, X, FileCode2, FileText } from 'lucide-react';

// Node colour map for search results (mirrors GitNexus Header.tsx)
const NODE_TYPE_COLORS: Record<string, string> = {
  file:      '#3b82f6',
  folder:    '#6366f1',
  function:  '#10b981',
  class:     '#f59e0b',
  method:    '#14b8a6',
  interface: '#ec4899',
  enum:      '#a78bfa',
};

function App() {
  const [loading, setLoading] = useState(false);
  const [progressMsg, setProgressMsg] = useState('');
  const [graphData, setGraphData] = useState<GraphData | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [searchQuery, setSearchQuery] = useState('');
  const [isSearchOpen, setIsSearchOpen] = useState(false);
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [selectedNodeLabel, setSelectedNodeLabel] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const searchRef    = useRef<HTMLDivElement>(null);
  const inputRef     = useRef<HTMLInputElement>(null);
  const codePanelRef = useRef<HTMLDivElement>(null);

  // Scroll to selected node in code panel
  useEffect(() => {
    if (selectedFile && selectedNodeLabel && codePanelRef.current) {
      // Find the text in the code and scroll to it
      // Simple implementation: search for the label in the code string, calculate line number
      const content = graphData?.files[selectedFile];
      if (content) {
         const lines = content.split('\n');
         const lineIdx = lines.findIndex(l => l.includes(selectedNodeLabel));
         if (lineIdx !== -1) {
            const lineEl = codePanelRef.current.querySelector(`[data-line="${lineIdx}"]`);
            if (lineEl) {
               lineEl.scrollIntoView({ behavior: 'smooth', block: 'center' });
               // Add a brief highlight effect
               lineEl.classList.add('bg-accent/20');
               setTimeout(() => lineEl.classList.remove('bg-accent/20'), 2000);
            }
         }
      }
    }
  }, [selectedFile, selectedNodeLabel, graphData]);

  /* ── search results across graph nodes ─────────────────────────────── */
  const searchResults = useMemo(() => {
    if (!graphData || !searchQuery.trim()) return [];
    const q = searchQuery.toLowerCase();
    return graphData.nodes
      .filter(n => n.label.toLowerCase().includes(q))
      .slice(0, 10);
  }, [graphData, searchQuery]);

  /* ── file ingestion ─────────────────────────────────────────────────── */
  const processFilesList = async (files: FileList | File[] | DataTransferItemList) => {
    if (!files || files.length === 0) return;
    setLoading(true);
    setProgressMsg('Reading files...');
    const fileEntries: FileData[] = [];
    const allowedExts = ['.js', '.jsx', '.ts', '.tsx', '.py', '.java', '.c', '.cpp', '.go', '.rs'];

    for (let i = 0; i < files.length; i++) {
      const item = files[i];
      let file: File | null = null;
      if ('getAsFile' in item) file = (item as DataTransferItem).getAsFile();
      else file = item as File;
      if (!file) continue;
      const path = file.webkitRelativePath || file.name;
      const isAllowed = allowedExts.some(ext => file!.name.endsWith(ext));
      if (path.includes('node_modules') || path.includes('.git/') || path.includes('dist/') || path.includes('target/') || !isAllowed) continue;
      try { fileEntries.push({ path, content: await file.text() }); }
      catch (err) { console.error(`Failed to read ${file.name}`, err); }
    }

    if (fileEntries.length === 0) {
      setLoading(false);
      alert('No source files found. Make sure you select a directory.');
      return;
    }
    try {
      const result = await processFiles(fileEntries, msg => setProgressMsg(msg));
      setGraphData(result);
    } catch (err) {
      console.error('Parsing failed:', err);
      alert('Failed during AST parsing. See console.');
    } finally { setLoading(false); }
  };

  const handleDirectorySelect = async (e: React.ChangeEvent<HTMLInputElement>) => {
    if (e.target.files) {
      await processFilesList(e.target.files);
      setSelectedFile(null);
      setSelectedNodeLabel(null);
    }
  };
  const handleDragOver  = (e: React.DragEvent) => { e.preventDefault(); setIsDragging(true); };
  const handleDragLeave = (e: React.DragEvent) => { e.preventDefault(); setIsDragging(false); };
  const handleDrop      = async (e: React.DragEvent) => {
    e.preventDefault(); setIsDragging(false);
    if (e.dataTransfer.items) {
      await processFilesList(e.dataTransfer.items as any);
    } else {
      await processFilesList(e.dataTransfer.files as any);
    }
    setSelectedFile(null);
    setSelectedNodeLabel(null);
  };

  const nodeCount = graphData?.nodes.length ?? 0;
  const edgeCount = graphData?.edges.length ?? 0;

  return (
    <div className="flex flex-col h-screen overflow-hidden bg-void text-text-primary font-sans">

      {/* ── Header ───────────────────────────────────────────────────────── */}
      <header className="flex items-center justify-between px-5 py-3 bg-deep border-b border-dashed border-border-subtle shrink-0 z-50">

        {/* Left: logo + project badge */}
        <div className="flex items-center gap-4">
          <div className="flex items-center gap-2.5">
            <div className="w-7 h-7 flex items-center justify-center bg-gradient-to-br from-accent to-accent-dim rounded-md shadow-glow text-white">
              <Box className="w-4 h-4" />
            </div>
            <span className="font-semibold text-[15px] tracking-tight">CodeGraphContext</span>
          </div>
          {graphData && (
            <button
              onClick={() => { setGraphData(null); setSelectedFile(null); setSelectedNodeLabel(null); }}
              className="flex items-center gap-2 px-3 py-1.5 bg-surface border border-border-subtle rounded-lg text-sm text-text-secondary hover:bg-hover transition-colors"
            >
              <span className="w-1.5 h-1.5 bg-node-function rounded-full animate-pulse" />
              <span>Playground</span>
            </button>
          )}
        </div>

        {/* Center: search (only when graph loaded) */}
        {graphData ? (
          <div className="flex-1 max-w-md mx-6 relative" ref={searchRef}>
            <div className="flex items-center gap-2.5 px-3.5 py-2 bg-surface border border-border-subtle rounded-lg transition-all focus-within:border-accent focus-within:ring-2 focus-within:ring-accent/20">
              <Search className="w-4 h-4 text-text-muted flex-shrink-0" />
              <input
                ref={inputRef}
                type="text"
                placeholder="Search nodes..."
                value={searchQuery}
                onChange={e => { setSearchQuery(e.target.value); setIsSearchOpen(true); }}
                onFocus={() => setIsSearchOpen(true)}
                onBlur={() => setTimeout(() => setIsSearchOpen(false), 150)}
                className="flex-1 bg-transparent border-none outline-none text-sm text-text-primary placeholder:text-text-muted"
              />
              <kbd className="px-1.5 py-0.5 bg-elevated border border-border-subtle rounded text-[10px] text-text-muted font-mono">⌘K</kbd>
            </div>

            {/* Search dropdown */}
            {isSearchOpen && searchQuery.trim() && (
              <div className="absolute top-full left-0 right-0 mt-1 bg-surface border border-border-subtle rounded-lg shadow-xl overflow-hidden z-50">
                {searchResults.length === 0 ? (
                  <div className="px-4 py-3 text-sm text-text-muted">No nodes found for "{searchQuery}"</div>
                ) : (
                  <div className="max-h-80 overflow-y-auto scrollbar-thin">
                    {searchResults.map(node => (
                      <button
                        key={node.id}
                        className="w-full px-4 py-2.5 flex items-center gap-3 text-left hover:bg-hover text-text-secondary transition-colors"
                        onClick={() => {
                          if (node.file) {
                             setSelectedFile(node.file);
                             setSelectedNodeLabel(node.label);
                          }
                          setIsSearchOpen(false);
                        }}
                      >
                        <span className="w-2.5 h-2.5 rounded-full flex-shrink-0"
                          style={{ backgroundColor: NODE_TYPE_COLORS[node.type] || '#6b7280' }} />
                        <span className="flex-1 truncate text-sm font-medium text-text-primary">{node.label}</span>
                        <span className="text-xs text-text-muted px-2 py-0.5 bg-elevated rounded">{node.type}</span>
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>
        ) : (
          <div className="flex-1" />
        )}

        {/* Right: icons */}
        <div className="flex items-center gap-2">
          {graphData && (
            <div className="flex items-center gap-4 mr-2 text-xs text-text-muted">
              <span>{nodeCount} nodes</span>
              <span>{edgeCount} edges</span>
            </div>
          )}
          <a
            href="https://github.com/Shashankss1205/CodeGraphContext"
            target="_blank" rel="noreferrer"
            className="flex items-center gap-2 px-3.5 py-2 bg-gradient-to-r from-purple-600 to-pink-600 hover:from-purple-500 hover:to-pink-500 rounded-lg text-white text-sm font-medium shadow-lg hover:shadow-xl hover:-translate-y-0.5 transition-all duration-200"
          >
            <Github className="w-4 h-4" />
            <span className="hidden sm:inline">Star on GitHub</span>
          </a>
          <button className="w-9 h-9 flex items-center justify-center rounded-md text-text-secondary hover:bg-hover hover:text-text-primary transition-colors">
            <Settings className="w-[18px] h-[18px]" />
          </button>
          <button className="w-9 h-9 flex items-center justify-center rounded-md text-text-secondary hover:bg-hover hover:text-text-primary transition-colors">
            <HelpCircle className="w-[18px] h-[18px]" />
          </button>
        </div>
      </header>

      {/* ── Main ─────────────────────────────────────────────────────────── */}
      <main className="flex-1 flex min-h-0">
        {graphData ? (
          <>
            {/* Sidebar Explorer */}
            <div className="w-64 border-r border-border-subtle bg-[#0d0d14] flex flex-col shrink-0 z-10 shadow-[4px_0_24px_rgba(0,0,0,0.2)]">
              <div className="px-4 py-3 text-xs font-semibold text-text-muted uppercase tracking-wider flex items-center gap-2 border-b border-border-subtle/50">
                <FileCode2 className="w-4 h-4 text-accent" />
                Explorer
              </div>
              <div className="flex-1 overflow-y-auto scrollbar-thin py-2">
                {Object.keys(graphData.files).sort().map(path => {
                  const isSelected = selectedFile === path;
                  const parts = path.split('/');
                  const name = parts[parts.length - 1];
                  const indent = Math.max(0, parts.length - 2) * 12;
                  
                  return (
                    <button
                      key={path}
                      onClick={() => { setSelectedFile(path); setSelectedNodeLabel(null); }}
                      className={`w-full text-left px-4 py-1.5 text-[13px] flex items-center gap-2 transition-colors relative
                        ${isSelected ? 'bg-accent/10 text-accent' : 'text-text-secondary hover:bg-white/5 hover:text-text-primary'}`}
                      style={{ paddingLeft: `${16 + indent}px` }}
                      title={path}
                    >
                      {isSelected && <div className="absolute left-0 top-0 bottom-0 w-0.5 bg-accent" />}
                      <FileText className="w-3.5 h-3.5 opacity-70 shrink-0" />
                      <span className="truncate">{name}</span>
                    </button>
                  );
                })}
              </div>
            </div>

            {/* Graph Canvas */}
            <div className="flex-1 relative min-w-0">
              <GraphCanvas 
                data={graphData} 
                onReset={() => { setGraphData(null); setSelectedFile(null); setSelectedNodeLabel(null); }}
                selectedFile={selectedFile}
                onNodeClick={(file, label) => {
                  setSelectedFile(file);
                  setSelectedNodeLabel(label);
                }}
              />
            </div>

            {/* Code Panel */}
            {selectedFile && graphData.files[selectedFile] && (
              <div className="w-[450px] border-l border-border-subtle bg-[#0a0a0f] flex flex-col shrink-0 z-10 shadow-[-4px_0_24px_rgba(0,0,0,0.2)]">
                <div className="flex items-center justify-between px-4 py-3 border-b border-border-subtle/50 bg-[#0d0d14]">
                  <div className="flex items-center gap-2 min-w-0">
                     <FileCode2 className="w-4 h-4 text-accent shrink-0" />
                     <span className="text-sm font-medium text-text-primary truncate" title={selectedFile}>
                       {selectedFile.split('/').pop()}
                     </span>
                     <span className="text-xs text-text-muted truncate ml-2 hidden sm:inline-block">
                       {selectedFile}
                     </span>
                  </div>
                  <button 
                    onClick={() => { setSelectedFile(null); setSelectedNodeLabel(null); }}
                    className="p-1 rounded-md text-text-muted hover:text-white hover:bg-white/10 transition-colors shrink-0"
                  >
                    <X className="w-4 h-4" />
                  </button>
                </div>
                <div 
                  ref={codePanelRef}
                  className="flex-1 overflow-y-auto scrollbar-thin p-4 text-[13px] font-mono leading-relaxed bg-[#0a0a0f] text-gray-300"
                >
                  {graphData.files[selectedFile].split('\n').map((line, i) => (
                    <div 
                      key={i} 
                      data-line={i}
                      className="flex hover:bg-white/5 px-2 -mx-2 rounded transition-colors"
                    >
                      <span className="w-8 shrink-0 text-right pr-4 text-gray-600 select-none">
                        {i + 1}
                      </span>
                      <span className="whitespace-pre-wrap word-break-all flex-1">
                        {line || ' '}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </>
        ) : (
          /* ── Onboarding / Dropzone ─────────────────────────────────── */
          <div className="flex-1 flex flex-col items-center justify-center p-6 relative overflow-hidden">
            {/* Ambient blobs */}
            <div className="absolute inset-0 pointer-events-none overflow-hidden">
              <div className="absolute -top-[20%] -right-[10%] w-[50%] h-[70%] rounded-[100%] bg-gradient-to-br from-accent/6 to-transparent blur-[140px] rotate-[-15deg]" />
              <div className="absolute -bottom-[20%] -left-[10%] w-[60%] h-[60%] rounded-[100%] bg-gradient-to-tr from-node-file/5 to-transparent blur-[120px]" />
            </div>

            <div
              onDragOver={handleDragOver}
              onDragLeave={handleDragLeave}
              onDrop={handleDrop}
              onClick={() => !loading && fileInputRef.current?.click()}
              className={`
                relative group flex flex-col items-center justify-center w-full max-w-2xl h-96
                border-2 border-dashed rounded-[2rem] overflow-hidden cursor-pointer z-10
                transition-all duration-500 hover:scale-[1.015]
                ${loading || isDragging
                  ? 'border-accent/60 bg-accent/8 shadow-glow-soft scale-[1.015]'
                  : 'border-border-default bg-surface/40 hover:border-accent/50 hover:bg-accent/5 hover:shadow-glow-soft'}
              `}
            >
              {/* Orb */}
              <div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 w-48 h-48 bg-accent blur-[120px] opacity-10 pointer-events-none group-hover:opacity-25 transition-opacity duration-700" />

              {loading ? (
                <div className="relative z-10 flex flex-col items-center gap-5">
                  <div className="relative w-20 h-20">
                    <div className="absolute inset-0 border-4 border-accent/20 rounded-full" />
                    <div className="absolute inset-0 border-4 border-accent rounded-full border-t-transparent animate-spin" />
                    <div className="absolute inset-2 border-2 border-accent-dim/40 rounded-full animate-spin" style={{ animationDirection: 'reverse', animationDuration: '1.5s' }} />
                    <Box className="absolute inset-0 m-auto w-7 h-7 text-accent animate-pulse" />
                  </div>
                  <div className="text-center">
                    <p className="text-base font-medium text-text-primary">{progressMsg}</p>
                    <p className="text-text-muted font-mono text-xs uppercase tracking-widest mt-1">Client-side WebAssembly · Tree-sitter</p>
                  </div>
                </div>
              ) : (
                <div className="relative z-10 flex flex-col items-center p-8 text-center gap-5">
                  <div className="w-20 h-20 rounded-2xl bg-elevated border border-border-subtle flex items-center justify-center shadow-xl group-hover:scale-110 group-hover:shadow-[0_0_25px_rgba(124,58,237,0.35)] group-hover:border-accent/50 transition-all duration-300">
                    <UploadCloud className="w-10 h-10 text-text-muted group-hover:text-accent transition-colors duration-300" />
                  </div>
                  <div>
                    <h1 className="text-3xl font-semibold text-text-primary tracking-tight">Visualize your Codebase</h1>
                    <p className="text-text-secondary mt-2 max-w-sm text-[15px] leading-relaxed">
                      Drop a local repository or select a directory. AST relationships are extracted
                      entirely client-side using WebAssembly — nothing leaves your machine.
                    </p>
                  </div>
                  <button className="px-8 py-3 bg-elevated hover:bg-accent text-text-primary border border-border-subtle hover:border-accent rounded-xl font-medium shadow-lg hover:shadow-glow transition-all duration-300 active:scale-95">
                    Select Local Directory
                  </button>
                </div>
              )}
            </div>

            <input
              type="file" ref={fileInputRef} onChange={handleDirectorySelect} className="hidden"
              {...({ webkitdirectory: 'true', directory: 'true' } as any)} multiple
            />
          </div>
        )}
      </main>

      {/* ── Status Bar (mirrors GitNexus StatusBar) ─────────────────────── */}
      <footer className="flex items-center justify-between px-5 py-2 bg-deep border-t border-dashed border-border-subtle text-[11px] text-text-muted shrink-0">
        {/* Left: status */}
        <div className="flex items-center gap-1.5">
          <span className="w-1.5 h-1.5 bg-node-function rounded-full" />
          <span>{loading ? progressMsg : 'Ready'}</span>
        </div>

        {/* Center: sponsor link */}
        <a
          href="https://github.com/Shashankss1205/CodeGraphContext"
          target="_blank" rel="noreferrer"
          className="group flex items-center gap-2 px-3 py-1 rounded-full bg-pink-500/10 border border-pink-500/20 hover:bg-pink-500/20 hover:border-pink-500/40 hover:scale-[1.02] transition-all duration-200"
        >
          <Heart className="w-3.5 h-3.5 text-pink-500 fill-pink-500/40 group-hover:fill-pink-500 transition-all duration-200 animate-pulse" />
          <span className="text-[11px] font-medium text-pink-400 group-hover:text-pink-300 transition-colors">Star us on GitHub</span>
        </a>

        {/* Right: stats */}
        <div className="flex items-center gap-3">
          {graphData && (
            <>
              <span>{nodeCount} nodes</span>
              <span className="text-border-default">•</span>
              <span>{edgeCount} edges</span>
            </>
          )}
        </div>
      </footer>
    </div>
  );
}

export default App;
