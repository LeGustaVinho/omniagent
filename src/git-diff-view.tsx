import { ChevronDown, ChevronRight, ChevronUp, Columns2, ExternalLink, FileCode2, FileImage, GitCompare, Rows3, X } from "lucide-react";
import type { editor } from "monaco-editor";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { loadMonaco, monacoTheme } from "@/ide-monaco";
import { ResizablePanel } from "@/panel-resize";
import { InlineIssue } from "@/tasks-git-ui";

export type GitTextComparison = {
  path: string;
  language: string;
  original: string;
  modified: string;
  patch?: string;
};

type DiffLayout = "auto" | "split" | "unified";

type GitDiffRow = {
  path: string;
  previousPath?: string;
  kind: string;
  additions?: number;
  deletions?: number;
};

export function GitDiffFrame({ source, title, summary, controls, onClose, children }: {
  source: string; title: string; summary?: ReactNode; controls?: ReactNode; onClose?: () => void; children: ReactNode;
}) {
  return <section className="delivery-view" aria-label="Changes">
    <header className="delivery-toolbar diff-toolbar">
      <GitCompare size={16} className="text-muted-foreground" aria-hidden />
      <h2 className="diff-source" data-source={source}>{title}</h2>
      {summary}
      {(controls || onClose) && <div className="ml-auto flex min-w-0 flex-wrap items-center gap-0.5">
        {controls}
        {onClose && <button type="button" className="delivery-tab" aria-label="Close changes" onClick={onClose}><X size={16} aria-hidden /></button>}
      </div>}
    </header>
    {children}
  </section>;
}

function ChangeCounts({ additions, deletions }: { additions: number; deletions: number }) {
  return <span className="flex gap-1.5 tabular-nums">
    <span className="text-[var(--success)]">+{additions}</span><span className="text-[var(--danger)]">−{deletions}</span>
  </span>;
}

export function GitDiffReview({ source, title, ranges, files, selectedPath, select, comparison, preview, empty, notice,
  allowSavedPatch, openFile, footer, onClose }: {
  source: string;
  title: string;
  ranges: { base: string; revised: string };
  files: readonly GitDiffRow[];
  selectedPath?: string;
  select: (path: string) => void;
  comparison?: GitTextComparison | null;
  preview?: ReactNode;
  empty: ReactNode;
  notice?: ReactNode;
  allowSavedPatch?: boolean;
  openFile?: () => void;
  footer?: ReactNode;
  onClose?: () => void;
}) {
  const [layout, setLayout] = useState<DiffLayout>("auto");
  const [patchPath, setPatchPath] = useState<string>();
  const [instance, setInstance] = useState<editor.IStandaloneDiffEditor | null>(null);
  const selected = files.find((file) => file.path === selectedPath);
  const hasPatch = Boolean(allowSavedPatch && comparison?.patch);
  const showingPatch = hasPatch && Boolean(selected && patchPath === selected.path);
  const counts = files.reduce<{ additions: number; deletions: number } | undefined>((sum, file) => {
    if (file.additions === undefined || file.deletions === undefined) return sum;
    return { additions: (sum?.additions ?? 0) + file.additions, deletions: (sum?.deletions ?? 0) + file.deletions };
  }, undefined);
  return <GitDiffFrame source={source} title={title} onClose={onClose}
    summary={counts && <span className="text-xs"><ChangeCounts {...counts} /></span>}
    controls={<>
      <button type="button" className="delivery-tab" aria-label="Previous change" title="Previous change"
        disabled={!instance || showingPatch} onClick={() => instance?.goToDiff("previous")}><ChevronUp size={15} aria-hidden /></button>
      <button type="button" className="delivery-tab" aria-label="Next change" title="Next change"
        disabled={!instance || showingPatch} onClick={() => instance?.goToDiff("next")}><ChevronDown size={15} aria-hidden /></button>
      <label className="diff-layout-select">
        {layout === "unified" ? <Rows3 size={14} aria-hidden /> : <Columns2 size={14} aria-hidden />}
        <select aria-label="Diff layout" className="delivery-filter" value={layout} disabled={!instance || showingPatch}
          onChange={(event) => setLayout(event.target.value as DiffLayout)}>
          <option value="auto">Auto</option><option value="split">Split</option><option value="unified">Unified</option>
        </select>
      </label>
    </>}>
    <div className="flex flex-wrap items-center gap-2 border-b border-border px-3 py-2 font-mono text-xs text-muted-foreground" aria-label="Comparison revisions">
      <span className="wrap-anywhere">{ranges.base}</span><span aria-label="to">→</span>
      <span className="font-medium wrap-anywhere text-foreground">{ranges.revised}</span>
    </div>
    {notice}
    {files.length === 0 ? <div className="delivery-empty"><GitCompare size={22} aria-hidden />{empty}</div>
      : <div className="flex min-h-0 flex-1 flex-col">
        <ResizablePanel as="section" className="diff-files" aria-label="Changed files"
          edge="bottom" resizeKey="git-changed-files" resizeLabel="changed files">
          <div className="diff-files-scroll">
          {files.map((file) => <button type="button" key={file.path} className="diff-file-row" aria-expanded={selected?.path === file.path}
            onClick={() => { select(selected?.path === file.path ? "" : file.path); setPatchPath(undefined); }}>
            {selected?.path === file.path ? <ChevronDown size={14} aria-hidden /> : <ChevronRight size={14} className="text-muted-foreground" aria-hidden />}
            {file.kind === "Binary" ? <FileImage size={14} className="text-muted-foreground" aria-hidden /> : <FileCode2 size={14} className="text-muted-foreground" aria-hidden />}
            <span className="min-w-0 flex-1 truncate font-mono" title={file.path}>{file.previousPath && `${file.previousPath} → `}{file.path}</span>
            <span className="text-muted-foreground">{file.kind}</span>
            {file.additions !== undefined && file.deletions !== undefined && file.kind !== "Binary" && <ChangeCounts additions={file.additions} deletions={file.deletions} />}
          </button>)}
          </div>
        </ResizablePanel>
        {selected && <div className="flex min-h-0 flex-1 flex-col">
          {(hasPatch || openFile) && <div className="flex flex-wrap items-center gap-1 border-b border-border px-2 py-1">
            {hasPatch && <button className="delivery-tab" type="button" aria-pressed={showingPatch} onClick={() => setPatchPath(showingPatch ? undefined : selected.path)}>{showingPatch ? "Compare side by side" : "View saved patch"}</button>}
            {openFile && <button className="delivery-tab" type="button" onClick={openFile}>Open file <ExternalLink size={12} aria-hidden className="ml-1 inline-block" /></button>}
          </div>}
          {selected.kind === "Binary"
            ? <div className="delivery-empty"><FileImage size={22} aria-hidden /><p>Binary file. Text comparison is unavailable.</p></div>
            : <div className="min-h-0 flex-1 overflow-auto">
              {preview ?? (comparison && (showingPatch ? <UnifiedPatch file={comparison} />
                : <MonacoComparison key={comparison.path} file={comparison} layout={layout} ready={setInstance} />))}
            </div>}
        </div>}
        {footer && <footer className="flex shrink-0 flex-wrap items-center gap-2 border-t border-border px-3 py-2">{footer}</footer>}
      </div>}
  </GitDiffFrame>;
}

function UnifiedPatch({ file }: { file: GitTextComparison }) {
  if (file.patch) return <pre className="min-h-40 overflow-auto p-3 font-mono text-xs leading-5">
    {file.patch.split("\n").map((line, index) => <span key={index} className={line.startsWith("+") ? "block bg-green-500/15" : line.startsWith("-") ? "block bg-red-500/15" : "block"}>{line || " "}</span>)}
  </pre>;
  return <div className="grid min-h-40 gap-3 overflow-auto p-3 sm:grid-cols-2">
    <section aria-label="Base file"><h3 className="mb-1 text-xs text-muted-foreground">Base</h3><pre className="overflow-auto rounded border border-border p-2 font-mono text-xs leading-5">{file.original || "Empty file"}</pre></section>
    <section aria-label="Revised file"><h3 className="mb-1 text-xs text-muted-foreground">Revised</h3><pre className="overflow-auto rounded border border-border p-2 font-mono text-xs leading-5">{file.modified || "Empty file"}</pre></section>
  </div>;
}

function MonacoComparison({ file, layout, ready }: {
  file: GitTextComparison; layout: DiffLayout; ready: (instance: editor.IStandaloneDiffEditor | null) => void;
}) {
  const anchor = useRef<HTMLDivElement>(null);
  const instance = useRef<editor.IStandaloneDiffEditor | null>(null);
  const [error, setError] = useState(false);
  const layoutSetting = useRef(layout);
  layoutSetting.current = layout;

  useEffect(() => {
    const host = anchor.current;
    if (!host) return;
    let active = true;
    let original: editor.ITextModel | undefined;
    let modified: editor.ITextModel | undefined;
    let themeObserver: MutationObserver | undefined;

    void loadMonaco().then((monaco) => {
      if (!active) return;
      const theme = () => monaco.editor.setTheme(monacoTheme());
      original = monaco.editor.createModel(file.original, file.language);
      modified = monaco.editor.createModel(file.modified, file.language);
      const opened = monaco.editor.createDiffEditor(host, {
        automaticLayout: true,
        readOnly: true,
        originalEditable: false,
        renderMarginRevertIcon: false,
        renderGutterMenu: false,
        renderIndicators: true,
        renderSideBySide: layoutSetting.current !== "unified",
        useInlineViewWhenSpaceIsLimited: layoutSetting.current === "auto",
        renderSideBySideInlineBreakpoint: 700,
        hideUnchangedRegions: { enabled: true, contextLineCount: 3 },
        ignoreTrimWhitespace: false,
        scrollBeyondLastLine: false,
        glyphMargin: false,
        folding: false,
        minimap: { enabled: false },
        renderOverviewRuler: false,
        fontFamily: '"Geist Mono Variable", Consolas, monospace',
        fontSize: 12,
        lineHeight: 20,
        diffWordWrap: "off",
        originalAriaLabel: `Base ${file.path}`,
        modifiedAriaLabel: `Revised ${file.path}`,
      });
      instance.current = opened;
      opened.setModel({ original, modified });
      theme();
      themeObserver = new MutationObserver(theme);
      themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-app-theme", "data-app-motion", "style", "class"] });
      ready(opened);
    }).catch(() => { if (active) setError(true); });

    return () => {
      active = false;
      ready(null);
      themeObserver?.disconnect();
      instance.current?.dispose();
      instance.current = null;
      original?.dispose();
      modified?.dispose();
    };
  }, [file, ready]);

  useEffect(() => {
    instance.current?.updateOptions({ renderSideBySide: layout !== "unified", useInlineViewWhenSpaceIsLimited: layout === "auto" });
  }, [layout]);

  return error ? <><div className="p-2"><InlineIssue>Diff editor unavailable. Showing the text comparison.</InlineIssue></div><UnifiedPatch file={file} /></>
    : <div className="h-full min-h-45 w-full min-w-0 overflow-hidden" ref={anchor} aria-label={`Diff editor for ${file.path}`} />;
}
