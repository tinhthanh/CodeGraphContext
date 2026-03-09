import React, { useEffect, useRef, useState, useCallback } from 'react';
import Graph from 'graphology';
import Sigma from 'sigma';
import FA2Layout from 'graphology-layout-forceatlas2/worker';
import forceAtlas2 from 'graphology-layout-forceatlas2';
import noverlap from 'graphology-layout-noverlap';
import EdgeCurveProgram from '@sigma/edge-curve';
import { ZoomIn, ZoomOut, Maximize2, X, Play, Pause } from 'lucide-react';

export interface GraphCanvasProps {
  data: {
    nodes: Array<{ id: string; label: string; type: string; file: string }>;
    edges: Array<{ id: string; source: string; target: string; type: string }>;
  };
  onReset?: () => void;
  selectedFile?: string | null;
  onNodeClick?: (file: string, label: string) => void;
}

/* ── Colors (mirrors GitNexus constants.ts exactly) ─────────────────────── */
const NODE_COLORS: Record<string, string> = {
  file:       '#3b82f6',
  folder:     '#6366f1',
  class:      '#f59e0b',
  interface:  '#ec4899',
  function:   '#10b981',
  method:     '#14b8a6',
  struct:     '#f97316',
  enum:       '#a78bfa',
  module:     '#22d3ee',
  namespace:  '#7c3aed',
  default:    '#6b7280',
};

const NODE_SIZES: Record<string, number> = {
  folder:     10,
  file:        6,
  class:       8,
  interface:   7,
  function:    4,
  method:      3,
  struct:      5,
  enum:        5,
  namespace:   9,
  module:     10,
  default:     4,
};

const EDGE_STYLES: Record<string, { color: string }> = {
  contains:   { color: '#2d5a3d' },  // forest green
  defines:    { color: '#0e7490' },  // cyan
  imports:    { color: '#1d4ed8' },  // blue
  calls:      { color: '#7c3aed' },  // violet
  inherits:   { color: '#c2410c' },  // orange
  implements: { color: '#be185d' },  // pink
};

const LEGEND_ENTRIES = [
  { label: 'Folder',     color: NODE_COLORS.folder },
  { label: 'File',       color: NODE_COLORS.file },
  { label: 'Class',      color: NODE_COLORS.class },
  { label: 'Function',   color: NODE_COLORS.function },
  { label: 'Interface',  color: NODE_COLORS.interface },
  { label: 'Method',     color: NODE_COLORS.method },
];

const getColor  = (type: string) => NODE_COLORS[type]  ?? NODE_COLORS.default;
const getSize   = (type: string) => NODE_SIZES[type]   ?? NODE_SIZES.default;

/* ── GitNexus helper functions (exact copy) ─────────────────────────────── */
const hexToRgb = (hex: string) => {
  const r = /^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(hex);
  return r ? { r: parseInt(r[1],16), g: parseInt(r[2],16), b: parseInt(r[3],16) } : { r:100,g:100,b:100 };
};
const rgbToHex = (r: number, g: number, b: number) =>
  '#' + [r,g,b].map(x => Math.max(0,Math.min(255,Math.round(x))).toString(16).padStart(2,'0')).join('');

const dimColor = (hex: string, amount: number) => {
  const { r,g,b } = hexToRgb(hex);
  const bg = { r:18, g:18, b:28 };
  return rgbToHex(bg.r+(r-bg.r)*amount, bg.g+(g-bg.g)*amount, bg.b+(b-bg.b)*amount);
};
const brightenColor = (hex: string, factor: number) => {
  const { r,g,b } = hexToRgb(hex);
  return rgbToHex(r+(255-r)*(factor-1)/factor, g+(255-g)*(factor-1)/factor, b+(255-b)*(factor-1)/factor);
};

/* ── FA2 settings (mirrors GitNexus EXACTLY) ─────── */
const getFA2Settings = (nodeCount: number) => {
  const isSmall  = nodeCount < 500;
  const isMedium = nodeCount >= 500 && nodeCount < 2000;
  const isLarge  = nodeCount >= 2000 && nodeCount < 10000;
  
  return {
    gravity:                      isSmall ? 0.8 : isMedium ? 0.5 : isLarge ? 0.3 : 0.15,
    scalingRatio:                 isSmall ? 15  : isMedium ? 30  : isLarge ? 60  : 100,
    slowDown:                     isSmall ? 1   : isMedium ? 2   : isLarge ? 3   : 5,
    barnesHutOptimize:            nodeCount > 200,
    barnesHutTheta:               isLarge || !isSmall && !isMedium ? 0.8 : 0.6,
    strongGravityMode:            false,
    outboundAttractionDistribution: true,
    linLogMode:                   false,
    adjustSizes:                  true,
    edgeWeightInfluence:          1,
  };
};

/* ── Component ──────────────────────────────────────────────────────────── */
export const GraphCanvas: React.FC<GraphCanvasProps> = ({ data, onReset, selectedFile, onNodeClick }) => {
  const containerRef   = useRef<HTMLDivElement>(null);
  const sigmaRef       = useRef<Sigma | null>(null);
  const graphRef       = useRef<Graph | null>(null);
  const layoutRef      = useRef<FA2Layout | null>(null);
  const selectedRef    = useRef<string | null>(null);

  const [isLayoutRunning, setIsLayoutRunning] = useState(true);
  const [selectedNode,    setSelectedNodeState] = useState<string | null>(null);
  const [hoveredLabel,    setHoveredLabel]      = useState<{ label: string; type: string } | null>(null);

  /* ── Camera helpers ─────────────────────────────────────────────────── */
  const zoomIn    = useCallback(() => sigmaRef.current?.getCamera().animatedZoom({ duration: 200 }), []);
  const zoomOut   = useCallback(() => sigmaRef.current?.getCamera().animatedUnzoom({ duration: 200 }), []);
  const resetView = useCallback(() => {
    sigmaRef.current?.getCamera().animatedReset({ duration: 300 });
    selectedRef.current = null;
    setSelectedNodeState(null);
    sigmaRef.current?.refresh();
  }, []);

  /* ── Build Sigma (once) ─────────────────────────────────────────────── */
  useEffect(() => {
    if (!containerRef.current) return;

    /* build graphology */
    const graph = new Graph();
    graphRef.current = graph;

    data.nodes.forEach(n => {
      if (graph.hasNode(n.id)) return;
      graph.addNode(n.id, {
        label:    n.label,
        size:     getSize(n.type),
        color:    getColor(n.type),
        x:        (Math.random() - 0.5) * 10,
        y:        (Math.random() - 0.5) * 10,
        nodeType: n.type,
        file:     n.file,
      });
    });

    data.edges.forEach(e => {
      if (!graph.hasNode(e.source) || !graph.hasNode(e.target)) return;
      if (graph.hasEdge(e.source, e.target)) return;
      const style = EDGE_STYLES[e.type] ?? { color: '#4a4a5a' };
      const curvature = 0.1 + Math.random() * 0.1;
      graph.addEdge(e.source, e.target, {
        size: 1.5,
        color: style.color,
        type: 'curved',
        curvature,
      });
    });

    /* sigma */
    const sigma = new Sigma(graph, containerRef.current!, {
      allowInvalidContainer:  true,
      defaultEdgeType:        'curved',
      edgeProgramClasses:     { curved: EdgeCurveProgram },
      renderLabels:           true,
      labelFont:              'JetBrains Mono, monospace',
      labelSize:              11,
      labelWeight:            '500',
      labelColor:             { color: '#e4e4ed' },
      labelRenderedSizeThreshold: 6,
      labelDensity:           0.1,
      labelGridCellSize:      70,
      defaultNodeColor:       '#6b7280',
      defaultEdgeColor:       'rgba(255, 255, 255, 0.2)', // make edges distinctly visible
      hideEdgesOnMove:        false, // <---- DO NOT HIDE EDGES while FA2 is running
      zIndex:                 true,
      minCameraRatio:         0.002,
      maxCameraRatio:         50,

      /* custom hover pill matching GitNexus */
      defaultDrawNodeHover: (context, data, settings) => {
        const label = data.label;
        if (!label) return;
        const size   = settings.labelSize || 11;
        const font   = settings.labelFont  || 'JetBrains Mono, monospace';
        const weight = settings.labelWeight || '500';
        context.font = `${weight} ${size}px ${font}`;
        const tw = context.measureText(label).width;
        const ns = data.size || 8;
        const x  = data.x, y = data.y - ns - 10;
        const px = 8, py = 5, h = size + py*2, w = tw + px*2;
        context.fillStyle = '#12121c';
        context.beginPath();
        context.roundRect(x - w/2, y - h/2, w, h, 4);
        context.fill();
        context.strokeStyle = (data as any).color || '#6366f1';
        context.lineWidth = 2;
        context.stroke();
        context.fillStyle = '#f5f5f7';
        context.textAlign = 'center';
        context.textBaseline = 'middle';
        context.fillText(label, x, y);
        context.beginPath();
        context.arc(data.x, data.y, ns + 4, 0, Math.PI * 2);
        context.strokeStyle = (data as any).color || '#6366f1';
        context.lineWidth = 2;
        context.globalAlpha = 0.5;
        context.stroke();
        context.globalAlpha = 1;
      },

      nodeReducer: (node, attrs) => {
        const res = { ...attrs };
        const sel = selectedRef.current;
        const fileSel = selectedFile;
        const g = graphRef.current;
        if (!g) return res;
        
        const belongsToFile = fileSel ? (attrs.file === fileSel || attrs.id === fileSel) : true;

        if (sel) {
          const isSelected = node === sel;
          const isNeighbor = g.hasEdge(node, sel) || g.hasEdge(sel, node);
          if (isSelected) {
            res.color = attrs.color; res.size = (attrs.size||8)*1.8; res.zIndex = 2; res.highlighted = true;
          } else if (isNeighbor) {
            res.color = attrs.color; res.size = (attrs.size||8)*1.3; res.zIndex = 1;
          } else {
            res.color = dimColor(attrs.color, 0.25); res.size = (attrs.size||8)*0.6; res.zIndex = 0;
          }
        } else if (fileSel) {
          if (belongsToFile) {
             res.color = attrs.color; res.zIndex = 1;
          } else {
             res.color = dimColor(attrs.color, 0.25); res.size = (attrs.size||8)*0.6; res.zIndex = 0;
          }
        }
        return res;
      },

      edgeReducer: (edge, attrs) => {
        const res = { ...attrs };
        const sel = selectedRef.current;
        const fileSel = selectedFile;
        const g = graphRef.current;
        if (!g) return res;
        
        const [src, tgt] = g.extremities(edge);
        const srcAttrs = g.getNodeAttributes(src);
        const tgtAttrs = g.getNodeAttributes(tgt);
        
        const belongsToFile = fileSel ? 
          (srcAttrs.file === fileSel || srcAttrs.id === fileSel || tgtAttrs.file === fileSel || tgtAttrs.id === fileSel) : true;

        if (sel) {
          const isConnected = src === sel || tgt === sel;
          if (isConnected) {
            res.color = brightenColor(attrs.color, 1.5);
            res.size  = Math.max(3, (attrs.size||1) * 4);
            res.zIndex = 2;
          } else {
            res.color = dimColor(attrs.color, 0.08);
            res.size  = 0.2;
            res.zIndex = 0;
          }
        } else if (fileSel) {
          if (belongsToFile) {
             res.color = brightenColor(attrs.color, 1.2);
             res.size  = Math.max(2, (attrs.size||1) * 2);
             res.zIndex = 1;
          } else {
             res.color = dimColor(attrs.color, 0.08);
             res.size  = 0.2;
             res.zIndex = 0;
          }
        }
        return res;
      },
    });

    sigmaRef.current = sigma;

    /* events */
    sigma.on('clickNode', ({ node }) => {
      const already = selectedRef.current === node;
      selectedRef.current = already ? null : node;
      setSelectedNodeState(already ? null : node);
      
      const a = graph.getNodeAttributes(node);
      if (!already && onNodeClick && a.file) {
        onNodeClick(a.file, a.label);
      }

      // tiny camera nudge to force edge re-render (GitNexus trick)
      const cam = sigma.getCamera();
      cam.animate({ ratio: cam.ratio * 1.0001 }, { duration: 50 });
      sigma.refresh();
    });
    sigma.on('clickStage', () => {
      selectedRef.current = null;
      setSelectedNodeState(null);
      sigma.refresh();
    });
    sigma.on('enterNode', ({ node }) => {
      const a = graph.getNodeAttributes(node);
      setHoveredLabel({ label: a.label, type: a.nodeType });
      if (containerRef.current) containerRef.current.style.cursor = 'pointer';
    });
    sigma.on('leaveNode', () => {
      setHoveredLabel(null);
      if (containerRef.current) containerRef.current.style.cursor = 'grab';
    });

    /* ForceAtlas2 worker layout — identical to GitNexus */
    const inferredSettings = forceAtlas2.inferSettings(graph);
    const customSettings   = getFA2Settings(graph.order);
    const settings = { ...inferredSettings, ...customSettings };
    const layout = new FA2Layout(graph, { settings });
    layoutRef.current = layout;
    layout.start();
    setIsLayoutRunning(true);

    /* stop after a duration based on graph size, then noverlap cleanup */
    const duration = graph.order < 500 ? 20000 : graph.order < 2000 ? 30000 : 35000;
    const timer = setTimeout(() => {
      layout.stop();
      layoutRef.current = null;
      noverlap.assign(graph, 20);
      sigma.refresh();
      setIsLayoutRunning(false);
    }, duration);

    return () => {
      clearTimeout(timer);
      layout.kill();
      sigma.kill();
      sigmaRef.current  = null;
      graphRef.current  = null;
      layoutRef.current = null;
    };
  }, [data]);

  return (
    <div className="relative w-full h-full bg-void flex-1">

      {/* Background gradient — exact GitNexus GraphCanvas */}
      <div className="absolute inset-0 pointer-events-none" style={{
        background: `radial-gradient(circle at 50% 50%, rgba(124,58,237,0.03) 0%, transparent 70%), linear-gradient(to bottom, #06060a, #0a0a10)`
      }} />

      {/* Sigma canvas — uses .sigma-container from GitNexus index.css */}
      <div ref={containerRef} className="sigma-container cursor-grab active:cursor-grabbing" />

      {/* Layout running indicator — green pill at bottom-center, exact GitNexus */}
      {isLayoutRunning && (
        <div className="absolute bottom-4 left-1/2 -translate-x-1/2 flex items-center gap-2 px-3 py-1.5 bg-emerald-500/20 border border-emerald-500/30 rounded-full backdrop-blur-sm z-30">
          <div className="w-2 h-2 bg-emerald-400 rounded-full animate-ping" />
          <span className="text-xs text-emerald-400 font-medium">Layout optimizing...</span>
        </div>
      )}

      {/* Node hover tooltip */}
      {hoveredLabel && (
        <div className="absolute top-4 left-1/2 -translate-x-1/2 z-30 pointer-events-none" style={{ marginTop: isLayoutRunning ? 32 : 0 }}>
          <div className="flex items-center gap-2 px-3 py-1.5 bg-[#12121c]/95 border border-[#2a2a3a] rounded-lg backdrop-blur-md shadow-xl">
            <div className="w-2 h-2 rounded-full" style={{ background: getColor(hoveredLabel.type) }} />
            <span className="text-sm font-mono text-white">{hoveredLabel.label}</span>
            <span className="text-xs text-[#8888a0] bg-[#1a1a2e] px-1.5 py-0.5 rounded">{hoveredLabel.type}</span>
          </div>
        </div>
      )}

      {/* Legend — bottom-left */}
      <div className="absolute bottom-4 left-4 z-20">
        <div className="flex flex-col gap-1.5 bg-[#12121c]/80 border border-[#2a2a3a] rounded-xl px-3 py-2.5 backdrop-blur-md">
          <p className="text-[10px] font-mono uppercase tracking-widest text-[#8888a0] mb-0.5">Node Types</p>
          {LEGEND_ENTRIES.map(e => (
            <div key={e.label} className="flex items-center gap-2">
              <div className="w-2 h-2 rounded-full flex-shrink-0" style={{ background: e.color }} />
              <span className="text-xs text-[#8888a0]">{e.label}</span>
            </div>
          ))}
        </div>
      </div>

      {/* Zoom + layout + reset controls — bottom-right (GitNexus style) */}
      <div className="absolute bottom-4 right-4 z-20 flex flex-col gap-1">
        <button onClick={zoomIn} title="Zoom In"
          className="w-9 h-9 flex items-center justify-center bg-elevated border border-border-subtle rounded-md text-text-secondary hover:bg-hover hover:text-text-primary transition-colors">
          <ZoomIn className="w-4 h-4" />
        </button>
        <button onClick={zoomOut} title="Zoom Out"
          className="w-9 h-9 flex items-center justify-center bg-elevated border border-border-subtle rounded-md text-text-secondary hover:bg-hover hover:text-text-primary transition-colors">
          <ZoomOut className="w-4 h-4" />
        </button>
        <button onClick={resetView} title="Fit to Screen"
          className="w-9 h-9 flex items-center justify-center bg-elevated border border-border-subtle rounded-md text-text-secondary hover:bg-hover hover:text-text-primary transition-colors">
          <Maximize2 className="w-4 h-4" />
        </button>

        <div className="h-px bg-border-subtle my-1" />

        {/* Play / Pause layout — identical to GitNexus */}
        <button
          onClick={() => {
            if (isLayoutRunning) {
              layoutRef.current?.stop();
              layoutRef.current = null;
              setIsLayoutRunning(false);
            } else if (graphRef.current) {
              const g = graphRef.current;
              const l = new FA2Layout(g, { settings: { ...forceAtlas2.inferSettings(g), ...getFA2Settings(g.order) } });
              layoutRef.current = l;
              l.start();
              setIsLayoutRunning(true);
            }
          }}
          title={isLayoutRunning ? 'Pause Layout' : 'Resume Layout'}
          className={`w-9 h-9 flex items-center justify-center border rounded-md transition-all
            ${isLayoutRunning
              ? 'bg-accent border-accent text-white shadow-glow animate-pulse'
              : 'bg-elevated border-border-subtle text-text-secondary hover:bg-hover hover:text-text-primary'}`}
        >
          {isLayoutRunning ? <Pause className="w-4 h-4" /> : <Play className="w-4 h-4" />}
        </button>

        <div className="h-px bg-border-subtle my-1" />

        <button onClick={onReset} title="Load new repo"
          className="w-9 h-9 flex items-center justify-center bg-elevated border border-border-subtle rounded-md text-text-secondary hover:bg-hover hover:text-text-primary transition-colors">
          <X className="w-4 h-4" />
        </button>
      </div>

      {/* Stats — top-right */}
      <div className="absolute top-4 right-4 z-20">
        <div className="flex gap-3 text-[11px] font-mono text-[#8888a0] bg-[#12121c]/80 border border-[#2a2a3a] px-3 py-1.5 rounded-lg backdrop-blur-md">
          <span>{data.nodes.length} nodes</span>
          <span className="text-[#3a3a4a]">·</span>
          <span>{data.edges.length} edges</span>
          {selectedFile && (
            <>
              <span className="text-[#3a3a4a]">·</span>
              <span className="text-[#3b82f6]">File: {selectedFile.split('/').pop()}</span>
            </>
          )}
          {selectedNode && (
            <>
              <span className="text-[#3a3a4a]">·</span>
              <span className="text-[#7c3aed]">Node: {selectedNode.split(':').pop()}</span>
            </>
          )}
        </div>
      </div>
    </div>
  );
};
