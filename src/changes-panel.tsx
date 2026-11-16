import { ChevronDown, ChevronUp, Columns2, ExternalLink, FileCode2, FileImage, GitCompare, Rows3, X } from "lucide-react";
import type { editor } from "monaco-editor";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { branchChanges, savedCandidate, workingChanges, type ChangedFile, type ChangesSource } from "@/git-demo-data";
import { loadMonaco, monacoTheme } from "@/ide-monaco";
import { useGitSimulation } from "@/git-simulation";
import { deliveryBase, deliveryBranch, deliveryCommit, deliveryRunId } from "@/tasks-demo-data";
import { DeliveryStatus, InlineIssue, IntegrationNotice } from "@/tasks-git-ui";
import type { WorkspaceProps } from "@/workbench-contract";


const sourceLabels: Record<ChangesSource, string> = {
  working: "Working tree", staged: "Staged", branch: "Branch", saved: "Saved diff",
};
const availableCandidate = ["review", "verified", "accepted", "merged"];

function UnifiedPatch({ file }: { file: ChangedFile }) {
  if (file.patch) return (
    <pre className="min-h-40 overflow-auto p-3 font-mono text-xs leading-5">
      {file.patch.split("\n").map((line, index) => <span key={index} className={line.startsWith("+") ? "block bg-green-500/15" : line.startsWith("-") ? "block bg-red-500/15" : "block"}>{line || " "}</span>)}
    </pre>
  );
  return (
    <div className="grid min-h-40 gap-3 overflow-auto p-3 sm:grid-cols-2">
      <section aria-label="Base file"><h3 className="mb-1 text-xs text-muted-foreground">Base</h3><pre className="overflow-auto rounded border border-border p-2 font-mono text-xs leading-5">{file.original || "Empty file"}</pre></section>
      <section aria-label="Revised file"><h3 className="mb-1 text-xs text-muted-foreground">Revised</h3><pre className="overflow-auto rounded border border-border p-2 font-mono text-xs leading-5">{file.modified || "Empty file"}</pre></section>
    </div>
  );
}

type DiffLayout = "auto" | "split" | "unified";

export function MonacoComparison({ file, layout, ready }: { file: ChangedFile; layout: DiffLayout; ready: (instance: editor.IStandaloneDiffEditor | null) => void }) {
  const anchor = useRef<HTMLDivElement>(null);
  const instance = useRef<editor.IStandaloneDiffEditor | null>(null);
  const [error, setError] = useState("");
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
      opened.setModel({ original, modified });
      instance.current = opened;
      theme();
      themeObserver = new MutationObserver(theme);
      themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-app-theme", "data-app-motion", "style", "class"] });
      ready(opened);
    }).catch((failure: unknown) => {
      if (active) setError(failure instanceof Error ? failure.message : String(failure));
    });

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

interface ChangesPanelProps extends WorkspaceProps {
  kind?: "saved" | "working";
  source?: ChangesSource;
  baseRef?: string;
  onClose?: () => void;
}

export default function ChangesPanel({
  simulation, startSimulation, target, kind, source, baseRef = "origin/main", onClose, openActivity, openWorkspace,
}: ChangesPanelProps) {
  const git = useGitSimulation();
  const stateBranch = git.worktrees.find((tree) => tree.id === git.checkout)?.branch ?? deliveryBranch;
  const liveFiles = git.checkout === "release" ? workingChanges.filter((file) => !git.committed.has(file.path)) : [];
  const activeSource = source ?? (kind === "saved" || target === "saved" ? "saved" : "working");
  const hasCandidate = simulation !== null && availableCandidate.includes(simulation.delivery);
  const files = !simulation ? []
    : activeSource === "saved" ? (hasCandidate ? savedCandidate : [])
    : activeSource === "staged" ? liveFiles.filter((file) => git.staged.has(file.path))
    : activeSource === "branch" ? (git.checkout === "release" ? branchChanges : [])
    : liveFiles;
  const [selection, setSelection] = useState<string>();
  const [layout, setLayout] = useState<DiffLayout>("auto");
  const [patch, setPatch] = useState(false);
  const [diffInstance, setDiffInstance] = useState<editor.IStandaloneDiffEditor | null>(null);
  const selected = selection === "" ? undefined : files.find((file) => file.path === selection) ?? files[0];
  const totals = files.reduce(({ additions, deletions }, file) => ({
    additions: additions + file.additions, deletions: deletions + file.deletions,
  }), { additions: 0, deletions: 0 });
  const label = sourceLabels[activeSource];
  const ranges = {
    working: { base: "HEAD", revised: `Worktree · ${stateBranch}` },
    staged: { base: "HEAD", revised: "Index" },
    branch: { base: `Merge base of ${baseRef}`, revised: `HEAD · ${stateBranch}` },
    saved: { base: deliveryBase, revised: deliveryCommit },
  }[activeSource];

  return (
    <section className="delivery-view" aria-label="Changes">
      <header className="delivery-toolbar diff-toolbar">
        <GitCompare size={16} className="text-muted-foreground" aria-hidden="true" />
        <h2 className="diff-source" data-source={activeSource}>{label}</h2>
        {simulation && <span className="flex gap-1.5 text-xs tabular-nums"><span className="text-[var(--success)]">+{totals.additions}</span><span className="text-[var(--danger)]">−{totals.deletions}</span></span>}
        <div className="ml-auto flex items-center gap-0.5">
          <button type="button" className="delivery-tab" aria-label="Previous change" title="Previous change" disabled={!diffInstance || patch} onClick={() => diffInstance?.goToDiff("previous")}><ChevronUp size={15} /></button>
          <button type="button" className="delivery-tab" aria-label="Next change" title="Next change" disabled={!diffInstance || patch} onClick={() => diffInstance?.goToDiff("next")}><ChevronDown size={15} /></button>
          <label className="diff-layout-select">
            {layout === "unified" ? <Rows3 size={14} aria-hidden="true" /> : <Columns2 size={14} aria-hidden="true" />}
            <select aria-label="Diff layout" className="delivery-filter" value={layout} disabled={!selected || selected.kind === "Binary" || patch}
              onChange={(event) => setLayout(event.target.value as DiffLayout)}>
              <option value="auto">Auto</option><option value="split">Split</option><option value="unified">Unified</option>
            </select>
          </label>
          {onClose && <button type="button" className="delivery-tab" aria-label="Close changes" onClick={onClose}><X size={16} /></button>}
        </div>
      </header>
      {simulation && <div className="flex flex-wrap items-center gap-2 border-b border-border px-3 py-2 font-mono text-xs text-muted-foreground" aria-label="Comparison revisions"><span>{ranges.base}</span><span aria-label="to">→</span><span className="font-medium text-foreground">{ranges.revised}</span></div>}
      {git.drift && <div className="border-b border-border px-3 py-2"><InlineIssue>{git.drift === "conflict" ? "Merge conflict. Resolve it in Git and run verification again." : "Destination changed. Recheck this version before committing further changes."}</InlineIssue></div>}
      {!simulation
        ? <div className="delivery-main"><IntegrationNotice title="Diff integration pending" onSimulate={startSimulation}>{activeSource === "saved" ? "Versioned changes appear here when the evidence service supplies a saved candidate." : "Git comparison will show the selected checkout after the Git service is connected."}</IntegrationNotice></div>
        : files.length === 0
          ? <div className="delivery-empty"><GitCompare size={22} aria-hidden="true" /><h3 className="font-semibold text-foreground">{activeSource === "saved" ? "No saved candidate" : "No changes"}</h3><p className="text-sm">{activeSource === "saved" ? "Capture an attempt to review its exact revision." : "Stage changes from the Git workspace to compare them here."}</p></div>
          : <div className="flex min-h-0 flex-1 flex-col">
            <section className="max-h-44 shrink-0 overflow-auto border-b border-border" aria-label="Changed files">
              {files.map((file) => <button type="button" key={file.path} className="diff-file-row" aria-expanded={selected?.path === file.path}
                onClick={() => { setSelection(selected?.path === file.path ? "" : file.path); setPatch(false); }}>
                {selected?.path === file.path ? <ChevronDown size={14} /> : <ChevronUp size={14} className="rotate-90 text-muted-foreground" />}
                {file.kind === "Binary" ? <FileImage size={14} className="text-muted-foreground" /> : <FileCode2 size={14} className="text-muted-foreground" />}
                <span className="min-w-0 flex-1 truncate font-mono" title={file.path}>{file.previousPath && `${file.previousPath} → `}{file.path}</span>
                <span className="text-muted-foreground">{file.kind}</span>
                {file.kind !== "Binary" && <span className="flex gap-1.5 tabular-nums"><span className="text-[var(--success)]">+{file.additions}</span><span className="text-[var(--danger)]">−{file.deletions}</span></span>}
              </button>)}
            </section>
            {selected && <div className="flex min-h-0 flex-1 flex-col">
              {(activeSource === "saved" ? selected.patch : selected.kind !== "Binary") && <div className="flex flex-wrap items-center gap-1 border-b border-border px-2 py-1">
                {activeSource === "saved" && selected.patch && <button className="delivery-tab" type="button" aria-pressed={patch} onClick={() => setPatch(!patch)}>{patch ? "Compare side by side" : "View saved patch"}</button>}
                {selected.kind !== "Binary" && activeSource !== "saved" && <button className="delivery-tab" type="button" onClick={() => openWorkspace(selected.path.endsWith(".md") ? "knowledge" : "ide", selected.path)}>Open file <ExternalLink size={12} className="ml-1 inline-block" /></button>}
              </div>}
              {selected.kind === "Binary"
                ? <div className="delivery-empty"><FileImage size={22} /><p className="text-sm">Binary file. Text comparison is unavailable.</p></div>
                : <div className="min-h-0 flex-1 overflow-auto">
                  {patch ? <UnifiedPatch file={selected} /> : <MonacoComparison key={selected.path} file={selected} layout={layout} ready={setDiffInstance} />}
                </div>}
            </div>}
            {simulation && activeSource === "saved" && <footer className="flex shrink-0 flex-wrap items-center gap-2 border-t border-border px-3 py-2">
              <Button className="px-2.5 py-1.5 text-xs" type="button" variant="outline" onClick={() => openActivity(deliveryRunId)}>View attempt</Button>
              {simulation.delivery === "review" && <Button className="px-2.5 py-1.5 text-xs" type="button" disabled={Boolean(git.drift)} onClick={() => simulation.advanceDelivery("verify")}>Run verification</Button>}
              {simulation.delivery === "verified" && <Button className="px-2.5 py-1.5 text-xs" type="button" disabled={Boolean(git.drift)} onClick={() => git.acceptCandidate(simulation)}>Accept diff</Button>}
              {simulation.delivery === "accepted" && <DeliveryStatus>Accepted</DeliveryStatus>}
              {simulation.delivery === "merged" && <DeliveryStatus>Merged</DeliveryStatus>}
            </footer>}
          </div>}
    </section>
  );
}
