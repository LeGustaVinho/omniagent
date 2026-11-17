import { ChevronDown, ChevronRight, ChevronUp, Columns2, ExternalLink, FileCode2, FileImage, GitCompare, Rows3, X } from "lucide-react";
import type { editor, IRange } from "monaco-editor";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { loadMonaco, monacoTheme } from "@/ide-monaco";
import { excerptReference, markdownExcerpt, type EditorExcerpt } from "@/editor-context";
import { ResizablePanel } from "@/panel-resize";
import { InlineIssue } from "@/tasks-git-ui";
import { ControlTooltip } from "@/components/ui/control-tooltip";
import { CopyFilePath, PathText } from "@/components/ui/file-path";
import { ContextActions, useContextActionFeedback, type MenuGroups } from "@/components/ui/context-actions";
import type { GitReviewAnnotations, GitReviewSide } from "@/git-review-comments";

export type GitTextComparison = {
  path: string;
  language: string;
  original: string;
  modified: string;
  patch?: string;
  hunks?: GitHunk[];
  originalPath?: string;
};

export type GitHunk = { id: string; header: string; patch: string; originalLine: number; modifiedLine: number };

type DiffLayout = "auto" | "split" | "unified";
type DiffSide = GitReviewSide;
type ExcerptHandler = (selection: EditorExcerpt, revision: string, instruction?: string) => void;

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
  allowSavedPatch, openFile, openLine, addExcerpt, fileActions, hunkActions, reviewAnnotations, reviewPanel, footer, onClose }: {
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
  openLine?: (line: number) => void;
  addExcerpt?: ExcerptHandler;
  fileActions?: (file: GitDiffRow) => MenuGroups;
  hunkActions?: (hunk: GitHunk) => MenuGroups;
  reviewAnnotations?: GitReviewAnnotations;
  reviewPanel?: ReactNode;
  footer?: ReactNode;
  onClose?: () => void;
}) {
  const [layout, setLayout] = useState<DiffLayout>("auto");
  const [patchPath, setPatchPath] = useState<string>();
  const [requestedLine, setRequestedLine] = useState<number>();
  const [copyError, setCopyError] = useState("");
  const [instance, setInstance] = useState<editor.IStandaloneDiffEditor | null>(null);
  const selected = files.find((file) => file.path === selectedPath);
  const hasHunks = Boolean(comparison?.hunks?.length);
  const hasPatch = Boolean(hasHunks || allowSavedPatch && comparison?.patch);
  const patchLineNumbers = useMemo(() => comparison ? {
    original: !/\r(?!\n)/.test(comparison.original),
    modified: !/\r(?!\n)/.test(comparison.modified),
  } : undefined, [comparison]);
  const [reviewLocation, setReviewLocation] = useState<GitReviewAnnotations["location"]>();
  const showingPatch = hasPatch && Boolean(selected && patchPath === selected.path);
  if (reviewLocation !== reviewAnnotations?.location) {
    setReviewLocation(reviewAnnotations?.location);
    if (reviewAnnotations?.location) {
      setPatchPath(undefined);
      if (reviewAnnotations.location.side === "original") setLayout("split");
    }
  }
  useEffect(() => {
    if (instance && requestedLine !== undefined) {
      const editor = instance.getModifiedEditor();
      const lineNumber = Math.max(1, requestedLine);
      editor.setPosition({ lineNumber, column: 1 });
      editor.revealLineInCenter(lineNumber);
      editor.focus();
      setRequestedLine(undefined);
    }
  }, [instance, requestedLine]);
  function copy(value: string) {
    setCopyError("");
    void Promise.resolve().then(() => navigator.clipboard.writeText(value)).catch(() => setCopyError("Could not copy to the clipboard."));
  }
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
    {reviewPanel}
    {copyError && <div className="p-3"><InlineIssue>{copyError}</InlineIssue></div>}
    {files.length === 0 ? <div className="delivery-empty"><GitCompare size={22} aria-hidden />{empty}</div>
      : <div className="flex min-h-0 flex-1 flex-col">
        <ResizablePanel as="section" className="diff-files" aria-label="Changed files"
          edge="bottom" resizeKey="git-changed-files" resizeLabel="changed files">
          <div className="diff-files-scroll">
          {files.map((file) => <ContextActions key={file.path} label={file.path} groups={[[
            { label: selected?.path === file.path ? "Collapse diff" : "Open diff", run: () => {
              select(selected?.path === file.path ? "" : file.path);
              setPatchPath(undefined);
            } },
            { label: "Copy relative path", run: () => copy(file.path) },
            ...(comparison?.path === file.path && comparison.patch !== undefined ? [
              { label: "Copy patch", run: () => copy(comparison.patch!) },
            ] : []),
          ], ...(fileActions?.(file) ?? [])]}>
            <button type="button" className="diff-file-row" title={file.previousPath ? `${file.previousPath} → ${file.path}` : file.path} aria-label={`${file.path} · ${file.kind}`} aria-expanded={selected?.path === file.path}
            onClick={() => { select(selected?.path === file.path ? "" : file.path); setPatchPath(undefined); }}>
            {selected?.path === file.path ? <ChevronDown size={14} aria-hidden /> : <ChevronRight size={14} className="text-muted-foreground" aria-hidden />}
            {file.kind === "Binary" ? <FileImage size={14} className="text-muted-foreground" aria-hidden /> : <FileCode2 size={14} className="text-muted-foreground" aria-hidden />}
            <span className="flex min-w-0 flex-1 font-mono">
              {file.previousPath && <span className="truncate">{file.previousPath} → </span>}<PathText path={file.path} />
            </span>
            <span className="text-muted-foreground">{file.kind}</span>
            {file.additions !== undefined && file.deletions !== undefined && file.kind !== "Binary" && <ChangeCounts additions={file.additions} deletions={file.deletions} />}
            </button>
          </ContextActions>)}
          </div>
        </ResizablePanel>
        {selected && <div className="flex min-h-0 flex-1 flex-col">
          <div className="flex flex-wrap items-center gap-1 border-b border-border px-2 py-1">
            <CopyFilePath path={selected.path} />
            {hasPatch && <button className="delivery-tab" type="button" aria-pressed={showingPatch} onClick={() => setPatchPath(showingPatch ? undefined : selected.path)}>
              {showingPatch ? "Compare side by side" : hasHunks ? "Review hunks" : "View saved patch"}
            </button>}
            {openFile && <button className="delivery-tab" type="button" onClick={openFile}>Open file <ExternalLink size={12} aria-hidden className="ml-1 inline-block" /></button>}
          </div>
          {selected.kind === "Binary"
            ? <div className="delivery-empty"><FileImage size={22} aria-hidden /><p>Binary file. Text comparison is unavailable.</p></div>
            : <div className="min-h-0 flex-1 overflow-auto">
              {preview ?? (comparison && (showingPatch ? (hasHunks ? <div className="grid gap-3 p-3" aria-label="Git hunks">
                {comparison.hunks!.map((hunk) => {
                  const goToHunk = () => { setRequestedLine(hunk.modifiedLine); setPatchPath(undefined); };
                  return <section key={hunk.id} className="min-w-0 rounded border border-border" aria-label={hunk.header}>
                    <ContextActions label={hunk.header} className="diff-hunk-menu" groups={[[
                      { label: "Go to hunk in comparison", run: goToHunk },
                      ...(openLine ? [{ label: "Go to hunk in working file", run: () => openLine(Math.max(1, hunk.modifiedLine)) }] : []),
                      { label: "Copy hunk patch", run: () => copy(hunk.patch) },
                      ...(hunk.modifiedLine > 0 ? [{ label: "Copy revised line reference", run: () => copy(`${comparison.path}:${hunk.modifiedLine}`) }] : []),
                    ], ...(hunkActions?.(hunk) ?? [])]}>
                      <div className="min-w-0 flex-1">
                        <button type="button" className="block w-full truncate border-b border-border bg-muted/30 p-2 text-left font-mono text-xs" onClick={goToHunk}>
                          {hunk.header}
                        </button>
                        <HunkPatchLines hunk={hunk} review={reviewAnnotations} lineNumbers={patchLineNumbers} />
                      </div>
                    </ContextActions>
                  </section>;
                })}
              </div> : <UnifiedPatch file={comparison} />)
                : <MonacoComparison key={comparison.path} file={comparison} layout={layout} ready={setInstance}
                  revisions={{ original: ranges.base, modified: ranges.revised }} openLine={openLine} addExcerpt={addExcerpt} review={reviewAnnotations} />))}
            </div>}
        </div>}
        {footer && <footer className="flex shrink-0 flex-wrap items-center gap-2 border-t border-border px-3 py-2">{footer}</footer>}
      </div>}
  </GitDiffFrame>;
}

function PatchLines({ value }: { value: string }) {
  return <pre className="overflow-auto p-3 font-mono text-xs leading-5">
    {value.split("\n").map((line, index) => <span key={index} className={line.startsWith("+") ? "block bg-green-500/15" : line.startsWith("-") ? "block bg-red-500/15" : "block"}>{line || " "}</span>)}
  </pre>;
}

function HunkPatchLines({ hunk, review, lineNumbers }: {
  hunk: GitHunk; review?: GitReviewAnnotations; lineNumbers?: Record<DiffSide, boolean>;
}) {
  const value = hunk.patch.slice(hunk.patch.indexOf("\n@@ ") + 1);
  const scope = review?.scope;
  if (!scope || !lineNumbers) return <PatchLines value={value} />;
  let original = hunk.originalLine;
  let modified = hunk.modifiedLine;
  return <div className="overflow-auto p-3 font-mono text-xs leading-5">
    {value.split("\n").map((line, index) => {
      const prefix = line[0];
      const originalLine = prefix === " " || prefix === "-" ? original++ : 0;
      const modifiedLine = prefix === " " || prefix === "+" ? modified++ : 0;
      const actions = ([["original", originalLine, "base"], ["modified", modifiedLine, "revised"]] as const)
        .filter(([side, number]) => lineNumbers[side] && number > 0 && number <= scope[side].lineCount)
        .map(([side, number, label]) => ({
          label: `Add review comment on ${label} line…`,
          run: () => review.add({ side, startLine: number, endLine: number }),
        }));
      const className = prefix === "+" ? "bg-green-500/15" : prefix === "-" ? "bg-red-500/15" : "";
      const label = [originalLine && `Base line ${originalLine}`, modifiedLine && `Revised line ${modifiedLine}`].filter(Boolean).join(" · ");
      return <ContextActions key={index} label={label} groups={[actions]} className={`diff-patch-line ${className}`}>
        <span className={`block min-w-0 flex-1 whitespace-pre ${className}`}>{line || " "}</span>
      </ContextActions>;
    })}
  </div>;
}

function UnifiedPatch({ file }: { file: GitTextComparison }) {
  if (file.patch) return <PatchLines value={file.patch} />;
  return <div className="grid min-h-40 gap-3 overflow-auto p-3 sm:grid-cols-2">
    <section aria-label="Base file"><h3 className="mb-1 text-xs text-muted-foreground">Base</h3><pre className="overflow-auto rounded border border-border p-2 font-mono text-xs leading-5">{file.original || "Empty file"}</pre></section>
    <section aria-label="Revised file"><h3 className="mb-1 text-xs text-muted-foreground">Revised</h3><pre className="overflow-auto rounded border border-border p-2 font-mono text-xs leading-5">{file.modified || "Empty file"}</pre></section>
  </div>;
}

export function MonacoComparison({ file, layout, ready, revisions, openLine, addExcerpt, review }: {
  file: GitTextComparison; layout: DiffLayout; ready?: (instance: editor.IStandaloneDiffEditor | null) => void;
  revisions?: Record<DiffSide, string>;
  openLine?: (line: number) => void;
  addExcerpt?: ExcerptHandler;
  review?: GitReviewAnnotations;
}) {
  const anchor = useRef<HTMLDivElement>(null);
  const instance = useRef<editor.IStandaloneDiffEditor | null>(null);
  const [opened, setOpened] = useState<editor.IStandaloneDiffEditor | null>(null);
  const [error, setError] = useState(false);
  const layoutSetting = useRef(layout);
  layoutSetting.current = layout;
  const { copy, feedback } = useContextActionFeedback();
  const actions = useRef({ revisions, openLine, addExcerpt, copy, review });
  actions.current = { revisions, openLine, addExcerpt, copy, review };

  useEffect(() => {
    const host = anchor.current;
    if (!host) return;
    const commentScope = actions.current.review?.scope;
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
        glyphMargin: Boolean(actions.current.review),
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
      for (const side of ["original", "modified"] as const) {
        const sideEditor = side === "original" ? opened.getOriginalEditor() : opened.getModifiedEditor();
        const path = side === "original" ? file.originalPath ?? file.path : file.path;
        let clickedRange: IRange | undefined;
        if (commentScope?.[side].lineCount) {
          sideEditor.onMouseDown(({ target, event }) => {
            clickedRange = undefined;
            if (target.type !== monaco.editor.MouseTargetType.GUTTER_GLYPH_MARGIN || event.rightButton) return;
            const current = actions.current.review;
            const comment = current?.comments.find((entry) => entry.side === side && entry.startLine === target.position?.lineNumber);
            if (comment) current?.show(comment.id);
          });
          sideEditor.onKeyDown(() => { clickedRange = undefined; });
          sideEditor.onContextMenu(({ target }) => {
            const position = target.position;
            clickedRange = position ? sideEditor.getSelections()?.find((range) => range.containsPosition(position)) : undefined;
          });
        }
        function selection(range: IRange | null = sideEditor.getSelection()): EditorExcerpt | undefined {
          const model = sideEditor.getModel();
          if (!range || !model) return;
          const { startLineNumber: startLine, endLineNumber, endColumn } = range;
          const endLine = endColumn === 1 && endLineNumber > startLine ? endLineNumber - 1 : endLineNumber;
          return { path, startLine, endLine, text: model.getValueInRange(range), language: file.language === "plaintext" ? undefined : file.language };
        }
        sideEditor.addAction({ id: `omniagent.copy-${side}-diff-line`, label: "Copy file and line reference", contextMenuGroupId: "9_omniagent-diff",
          run: () => {
            const range = selection();
            const revision = actions.current.revisions?.[side];
            if (range) actions.current.copy([excerptReference(range), revision].filter(Boolean).join(" · "));
          } });
        sideEditor.addAction({ id: `omniagent.copy-${side}-diff-markdown`, label: "Copy selection as Markdown", contextMenuGroupId: "9_omniagent-diff",
          precondition: "editorHasSelection", run: () => { const range = selection(); if (range?.text) actions.current.copy(markdownExcerpt(range)); } });
        if (actions.current.addExcerpt) for (const [id, label, instruction] of [
          ["quote", "Add diff selection to conversation…", undefined],
          ["review", "Review selected change…", "Review this excerpt in the context of the Git comparison."],
        ] as const) sideEditor.addAction({ id: `omniagent.${id}-${side}-diff-selection`, label, contextMenuGroupId: "9_omniagent-diff",
          precondition: "editorHasSelection", run: () => {
            const range = selection();
            if (range?.text) actions.current.addExcerpt?.(range, actions.current.revisions?.[side] ?? (side === "original" ? "Base" : "Revised"), instruction);
          } });
        if (side === "modified" && actions.current.openLine) sideEditor.addAction({ id: "omniagent.open-working-diff-line",
          label: "Go to line in working file", contextMenuGroupId: "9_omniagent-diff",
          run: () => { const line = sideEditor.getPosition()?.lineNumber; if (line) actions.current.openLine?.(line); } });
        if (commentScope?.[side].lineCount) sideEditor.addAction({
          id: `omniagent.review-comment-${side}-diff`, label: "Add review comment…", contextMenuGroupId: "9_omniagent-diff",
          run: () => {
            const range = selection(clickedRange);
            clickedRange = undefined;
            const current = actions.current.review;
            if (range && current?.scope?.snapshot === commentScope.snapshot) current.add({ side, startLine: range.startLine, endLine: range.endLine });
          },
        });
      }
      theme();
      themeObserver = new MutationObserver(theme);
      themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-app-theme", "data-app-motion", "style", "class"] });
      ready?.(opened);
      setOpened(opened);
    }).catch(() => { if (active) setError(true); });

    return () => {
      active = false;
      ready?.(null);
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

  useEffect(() => {
    if (!opened || opened !== instance.current) return;
    const decorations = (["original", "modified"] as const).map((side) => {
      const target = side === "original" ? opened.getOriginalEditor() : opened.getModifiedEditor();
      return target.createDecorationsCollection((review?.comments ?? []).filter((comment) => comment.side === side).flatMap((comment) => [{
        range: { startLineNumber: comment.startLine, startColumn: 1, endLineNumber: comment.endLine, endColumn: 1 },
        options: {
          isWholeLine: true,
          className: "diff-review-comment-lines",
          glyphMarginHoverMessage: { value: markdownExcerpt({ text: comment.body.slice(0, 800) }) },
          hoverMessage: { value: markdownExcerpt({ text: comment.body.slice(0, 800) }) },
        },
      }, {
        range: { startLineNumber: comment.startLine, startColumn: 1, endLineNumber: comment.startLine, endColumn: 1 },
        options: { glyphMarginClassName: "diff-review-comment-marker", glyphMarginHoverMessage: { value: "Open review comment" } },
      }]));
    });
    return () => { if (opened === instance.current) decorations.forEach((collection) => collection.clear()); };
  }, [opened, review?.comments]);

  useEffect(() => {
    const location = review?.location;
    if (!opened || opened !== instance.current || !location) return;
    opened.updateOptions({ hideUnchangedRegions: { enabled: false } });
    const target = location.side === "original" ? opened.getOriginalEditor() : opened.getModifiedEditor();
    const endColumn = target.getModel()?.getLineMaxColumn(location.endLine) ?? 1;
    target.setSelection({ startLineNumber: location.startLine, startColumn: 1, endLineNumber: location.endLine, endColumn });
    target.revealLineInCenter(location.startLine);
    target.focus();
  }, [opened, review?.location]);

  return error ? <><div className="p-2"><InlineIssue>Diff editor unavailable. Showing the text comparison.</InlineIssue></div><UnifiedPatch file={file} /></>
    : <div className="flex h-full min-h-45 w-full min-w-0 flex-col overflow-hidden">
      {feedback && <p className="px-3 py-1 text-xs" role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      <div className="min-h-0 flex-1" ref={anchor} aria-label={`Diff editor for ${file.path}`} />
    </div>;
}
