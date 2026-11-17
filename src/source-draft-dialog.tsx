import { useMutation, useQuery } from "@tanstack/react-query";
import { lazy, Suspense } from "react";
import { FileDialog } from "@/ide-file-workspace";
import type { SavedDraft } from "@/repository-note-buffers";

const MonacoComparison = lazy(() => import("@/git-diff-view").then(({ MonacoComparison }) => ({ default: MonacoComparison })));

export function SourceDraftDialog({ checkoutId, path, base, editorText, read, restore, copy, close }: {
  checkoutId: string; path: string; base: string; editorText: string;
  read: () => Promise<SavedDraft | null>; restore: (reviewed: SavedDraft) => Promise<void>;
  copy: (text: string) => void; close: () => void;
}) {
  const draft = useQuery({ queryKey: ["protected-source-draft", checkoutId, path], queryFn: read, gcTime: 0, retry: false,
    networkMode: "always", refetchOnWindowFocus: false });
  const apply = useMutation({ mutationFn: restore, onSuccess: close, networkMode: "always" });
  const reviewed = draft.data;
  const sameBase = reviewed?.base.id === base;
  const changed = Boolean(reviewed && reviewed.markdown !== editorText);
  return <FileDialog title="Review protected draft" description={path} className="file-comparison-dialog"
    onClose={() => { if (!apply.isPending) close(); }}>
    {draft.isPending && <p role="status">Reading protected draft…</p>}
    {draft.error && <p role="alert">{String(draft.error)}</p>}
    {reviewed === null && <p role="status">This file has no protected draft.</p>}
    {reviewed && <>
      <p className="text-xs">Draft revision <code>{reviewed.revision}</code></p>
      {!sameBase && <p role="status">The draft is based on a different saved revision. Reopen the file to recover it with its saved base.</p>}
      {!changed && <p role="status">The protected draft already matches the editor.</p>}
      <div className="file-comparison-body min-h-40">
        <Suspense fallback={<p role="status">Loading comparison…</p>}>
          <MonacoComparison layout="split" file={{ path, language: "plaintext", original: editorText, modified: reviewed.markdown }}
            revisions={{ original: "Current editor", modified: "Protected draft" }} />
        </Suspense>
      </div>
    </>}
    {apply.error && <p role="alert">{String(apply.error)}</p>}
    <div className="file-dialog-footer">
      <button type="button" className="fw-btn" disabled={apply.isPending} onClick={close}>Close</button>
      {reviewed && <button type="button" className="fw-btn" onClick={() => copy(reviewed.markdown)}>Copy protected draft</button>}
      {reviewed && sameBase && changed && <button type="button" className="fw-btn fw-primary" disabled={apply.isPending}
        onClick={() => apply.mutate(reviewed)}>Restore draft to editor</button>}
    </div>
  </FileDialog>;
}
