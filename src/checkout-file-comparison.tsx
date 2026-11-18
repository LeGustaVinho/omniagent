import { invoke } from "@tauri-apps/api/core";
import { lazy, Suspense, useRef, useState } from "react";
import { CheckoutDocumentHistory } from "@/checkout-document-history";
import type { MenuAction } from "@/components/ui/context-actions";
import type { GitTextComparison } from "@/git-diff-view";
import type { RevisionComparisonTarget } from "@/git-file-history";
import { compareGitFileVersions, gitFileText, type GitFileVersion } from "@/git-file-versions";
import { FileDialog, type FilePanelApi, type FileTreeTarget } from "@/ide-file-workspace";

type TextState = { content: string; saved: string; external?: string };
type Comparison = { labels: [string, string]; ownerPanel?: FilePanelApi; file?: GitTextComparison; error?: string };
const MonacoComparison = lazy(() => import("@/git-diff-view").then(({ MonacoComparison }) => ({ default: MonacoComparison })));
const FileRevisionComparison = lazy(() => import("@/git-file-history").then(({ FileRevisionComparison }) => ({ default: FileRevisionComparison })));

export function useCheckoutFileComparison({ checkoutId, bufferAt, openHistory, restoreLocalText, canRestore }: {
  checkoutId: string;
  bufferAt: (path: string) => TextState | undefined;
  openHistory?: (path: string) => void;
  restoreLocalText?: (path: string, text: string) => Promise<void>;
  canRestore?: (path: string) => boolean;
}) {
  const [selected, setSelected] = useState<string>();
  const [comparison, setComparison] = useState<Comparison>();
  const [history, showHistory] = useState<{ path: string; ownerPanel?: FilePanelApi }>();
  const [revision, selectRevision] = useState<{ path: string; target: RevisionComparisonTarget; ownerPanel?: FilePanelApi }>();
  const [layout, setLayout] = useState<"split" | "unified">("split");
  const request = useRef(0);

  async function readDisk(path: string) {
    const note = /\.(?:md|markdown)$/i.test(path);
    const open = await invoke<{ status: string; document?: { markdown: string } }>(note ? "open_repository_document" : "open_repository_source_file", { checkoutId, path });
    if (open.status !== "document" || !open.document) throw new Error(`Could not read ${path} as text.`);
    return open.document.markdown;
  }

  async function read(path: string) {
    return bufferAt(path)?.content ?? readDisk(path);
  }

  async function compare(labels: [string, string], input: [Promise<string>, Promise<string>], path: string, ownerPanel?: FilePanelApi) {
    const id = ++request.current;
    setComparison({ labels, ownerPanel });
    try {
      const [original, modified] = await Promise.all(input);
      if (request.current === id) setComparison({ labels, ownerPanel, file: { path, language: /\.(?:md|markdown)$/i.test(path) ? "markdown" : "plaintext", original, modified } });
    } catch (failure) {
      if (request.current === id) setComparison({ labels, ownerPanel, error: String(failure) });
    }
  }

  function comparePaths(left: string, right: string, ownerPanel?: FilePanelApi) {
    void compare([left, right], [read(left), read(right)], right, ownerPanel);
  }

  function compareDisk(path: string, base: GitFileVersion, revised: GitFileVersion, labels: [string, string], ownerPanel?: FilePanelApi) {
    const comparison = compareGitFileVersions(checkoutId, path, base, revised);
    void compare(labels, [comparison.then(({ original }) => gitFileText(original)), comparison.then(({ modified }) => gitFileText(modified))], path, ownerPanel);
  }

  function compareDraft(path: string, base: "head" | "index", content: string, ownerPanel?: FilePanelApi) {
    const version = compareGitFileVersions(checkoutId, path, null, { kind: base });
    void compare([base === "head" ? "HEAD" : "Git index", `${path} · Editor`],
      [version.then(({ modified }) => gitFileText(modified)), Promise.resolve(content)], path, ownerPanel);
  }

  function actions(path: string, ownerPanel?: FilePanelApi): MenuAction[] {
    const local = bufferAt(path);
    return [
      { label: selected === path ? "Clear comparison selection" : "Select for comparison", run: () => setSelected(selected === path ? undefined : path) },
      ...(selected && selected !== path ? [{ label: "Compare with selected file", run: () => comparePaths(selected, path, ownerPanel) }] : []),
      ...(local && local.content !== local.saved ? [{ label: "Compare with saved version", run: () => {
        void compare([`${path} · Saved`, `${path} · Editor`], [Promise.resolve(local.saved), Promise.resolve(local.content)], path, ownerPanel);
      } }] : []),
      ...(local?.external !== undefined ? [{ label: "Compare editor with file on disk", run: () => {
        void compare([`${path} · Disk`, `${path} · Editor`], [Promise.resolve(local.external!), Promise.resolve(local.content)], path, ownerPanel);
      } }] : []),
      { label: "Compare Git versions", items: [
        { label: "HEAD with staged index", run: () => compareDisk(path, { kind: "head" }, { kind: "index" }, ["HEAD", "Git index"], ownerPanel) },
        { label: "HEAD with file on disk", run: () => compareDisk(path, { kind: "head" }, { kind: "worktree" }, ["HEAD", `${path} · Disk`], ownerPanel) },
        { label: "Staged index with file on disk", run: () => compareDisk(path, { kind: "index" }, { kind: "worktree" }, ["Git index", `${path} · Disk`], ownerPanel) },
        { label: "Compare file on disk with branch or commit…", run: () => selectRevision({ path, target: { kind: "disk" }, ownerPanel }) },
        ...(local ? [
          { label: "HEAD with editor", run: () => compareDraft(path, "head", local.content, ownerPanel) },
          { label: "Staged index with editor", run: () => compareDraft(path, "index", local.content, ownerPanel) },
          { label: "Compare editor with branch or commit…", run: () => selectRevision({ path, target: { kind: "editor", text: local.content }, ownerPanel }) },
        ] : []),
      ] },
      ...(openHistory ? [{ label: "Show file history", run: () => openHistory(path) }] : []),
      { label: "Show local history", run: () => showHistory({ path, ownerPanel }) },
    ];
  }

  function selectionActions(items: readonly FileTreeTarget[]): MenuAction[] {
    if (items.length !== 2 || !items.every(({ kind }) => kind === "file")) return [];
    return [{ label: "Compare selected files", run: () => comparePaths(items[0]!.path, items[1]!.path) }];
  }

  const dialog = comparison && <FileDialog title="Compare files" description={comparison.labels.join(" ↔ ")} className="file-comparison-dialog" ownerPanel={comparison.ownerPanel}
    onClose={() => { request.current += 1; setComparison(undefined); }}>
    <div className="file-comparison-body">
      {comparison.error ? <p role="alert">{comparison.error}</p> : comparison.file ? <>
        <label className="flex items-center gap-2">Diff layout <select className="delivery-filter" value={layout}
          onChange={(event) => setLayout(event.currentTarget.value as "split" | "unified")}>
          <option value="split">Split</option><option value="unified">Unified</option>
        </select></label>
        <div className="min-h-0 flex-1 overflow-auto">
          <Suspense fallback={<p role="status">Loading comparison…</p>}>
            <MonacoComparison file={comparison.file} layout={layout} />
          </Suspense>
        </div>
      </> : <p role="status">Loading file content…</p>}
    </div>
  </FileDialog>;

  return { actions, selectionActions, dialog: revision ? <FileDialog title={revision.target.kind === "disk" ? "Compare file on disk with Git revision" : "Compare editor with Git revision"}
    description={revision.path} ownerPanel={revision.ownerPanel} className="file-comparison-dialog" onClose={() => selectRevision(undefined)}>
    <Suspense fallback={<p role="status">Loading Git comparison…</p>}>
      <FileRevisionComparison key={`${revision.target.kind}:${revision.path}`} checkoutId={checkoutId} path={revision.path} target={revision.target} />
    </Suspense>
  </FileDialog>
    : history ? <CheckoutDocumentHistory key={history.path} checkoutId={checkoutId} path={history.path}
    editorText={bufferAt(history.path)?.content} readDisk={() => readDisk(history.path)} ownerPanel={history.ownerPanel}
    restore={restoreLocalText && canRestore?.(history.path) !== false ? (text) => restoreLocalText(history.path, text) : undefined}
    onClose={() => showHistory(undefined)} /> : dialog };
}
