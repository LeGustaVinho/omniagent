import { invoke } from "@tauri-apps/api/core";
import { lazy, Suspense, useRef, useState } from "react";
import type { MenuAction } from "@/components/ui/context-actions";
import type { GitTextComparison } from "@/git-diff-view";
import { FileDialog, type FileTreeTarget } from "@/ide-file-workspace";
import { fileName } from "@/ide-knowledge-data";

type TextState = { content: string; saved: string; external?: string };
type Comparison = { labels: [string, string]; file?: GitTextComparison; error?: string };
const MonacoComparison = lazy(() => import("@/git-diff-view").then(({ MonacoComparison }) => ({ default: MonacoComparison })));

export function useCheckoutFileComparison({ checkoutId, simulated, bufferAt, copy, report }: {
  checkoutId: string;
  simulated: boolean;
  bufferAt: (path: string) => TextState | undefined;
  copy: (text: string, message: string) => void;
  report: (message: string) => void;
}) {
  const [selected, setSelected] = useState<string>();
  const [comparison, setComparison] = useState<Comparison>();
  const [layout, setLayout] = useState<"split" | "unified">("split");
  const request = useRef(0);

  async function read(path: string) {
    const local = bufferAt(path);
    if (local) return local.content;
    if (simulated) throw new Error(`No text is available for ${path}.`);
    const note = /\.(?:md|markdown)$/i.test(path);
    const open = await invoke<{ status: string; document?: { markdown: string } }>(note ? "open_repository_document" : "open_repository_source_file", { checkoutId, path });
    if (open.status !== "document" || !open.document) throw new Error(`Could not read ${path} as text.`);
    return open.document.markdown;
  }

  async function compare(labels: [string, string], input: [Promise<string>, Promise<string>], path: string) {
    const id = ++request.current;
    setComparison({ labels });
    try {
      const [original, modified] = await Promise.all(input);
      if (request.current === id) setComparison({ labels, file: { path, language: /\.(?:md|markdown)$/i.test(path) ? "markdown" : "plaintext", original, modified } });
    } catch (failure) {
      if (request.current === id) setComparison({ labels, error: String(failure) });
    }
  }

  function comparePaths(left: string, right: string) {
    void compare([left, right], [read(left), read(right)], right);
  }

  function actions(path: string): MenuAction[] {
    const local = bufferAt(path);
    const name = fileName(path);
    return [
      { label: "Copy text content", run: () => { void read(path).then((text) => copy(text, `Copied ${name}`), (failure: unknown) => report(String(failure))); } },
      { label: selected === path ? "Clear comparison selection" : "Select for comparison", run: () => setSelected(selected === path ? undefined : path) },
      ...(selected && selected !== path ? [{ label: "Compare with selected file", run: () => comparePaths(selected, path) }] : []),
      ...(local && local.content !== local.saved ? [{ label: "Compare with saved version", run: () => {
        void compare([`${path} · Saved`, `${path} · Editor`], [Promise.resolve(local.saved), Promise.resolve(local.content)], path);
      } }] : []),
    ];
  }

  function selectionActions(items: readonly FileTreeTarget[]): MenuAction[] {
    if (items.length !== 2 || !items.every(({ kind }) => kind === "file")) return [];
    return [{ label: "Compare selected files", run: () => comparePaths(items[0]!.path, items[1]!.path) }];
  }

  const dialog = comparison && <FileDialog title="Compare files" description={comparison.labels.join(" ↔ ")} className="file-comparison-dialog"
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

  return { actions, selectionActions, dialog };
}
