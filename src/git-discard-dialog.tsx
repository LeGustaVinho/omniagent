import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { lazy, Suspense, useState } from "react";
import { Button } from "@/components/ui/button";
import { publishCheckoutItemChanges } from "@/checkout-item-operations";
import { FileDialog } from "@/ide-file-workspace";
import { InlineIssue } from "@/tasks-git-ui";

type PreviewFile = { status: "missing" } | { status: "text"; content: string }
  | { status: "bytes"; size: number; sha256: string; reason: string };
type Comparison = { path: string; before: PreviewFile; after: PreviewFile };
type Preview = Comparison & { snapshot: string; hunk: string | null };
type BatchPreview = { snapshot: string; files: Comparison[] };
export type GitDiscardSelection = { path: string; hunkId?: string; untracked?: never }
  | { path: string; hunkId?: never; untracked: true };

const MonacoComparison = lazy(() => import("@/git-diff-view").then(({ MonacoComparison }) => ({ default: MonacoComparison })));

export function GitDiscardDialog({ checkoutId, selection, close, onRecycled }: {
  checkoutId: string; selection: GitDiscardSelection; close: () => void; onRecycled?: (path: string) => void;
}) {
  const client = useQueryClient();
  const recycle = selection.untracked === true;
  const args = { checkoutId, path: selection.path, hunkId: selection.hunkId ?? null, ...(recycle ? { untracked: true } : {}) };
  const review = useQuery({
    queryKey: ["git-discard-preview", args], queryFn: () => invoke<Preview>("git_discard_preview", args),
    retry: false, refetchOnMount: "always", refetchOnWindowFocus: false,
  });
  const apply = useMutation({
    mutationFn: (snapshot: string) => invoke("git_discard_changes", { ...args, snapshot }),
    onSuccess: () => {
      if (recycle) {
        publishCheckoutItemChanges(checkoutId, "recycle", [{ source: selection.path, directory: false, destination: null }]);
        onRecycled?.(selection.path);
      }
      void client.invalidateQueries({ queryKey: ["git-review", checkoutId] });
      void client.invalidateQueries({ queryKey: ["git-live-diff", checkoutId] });
      close();
    },
  });
  const preview = review.data;
  const restore = preview?.before.status === "missing";
  const action = recycle ? "Move untracked file to Recycle Bin"
    : selection.hunkId ? "Discard hunk" : restore ? "Restore deleted file" : "Discard file changes";
  return <FileDialog title={action} description={selection.path} className="file-comparison-dialog"
    onClose={() => { if (!apply.isPending) close(); }}>
    {review.isFetching && <p role="status">{recycle ? "Reviewing the untracked file…" : "Preparing the result from the Git index…"}</p>}
    {review.error && <InlineIssue>{String(review.error)}</InlineIssue>}
    {apply.error && <InlineIssue>{String(apply.error)}</InlineIssue>}
    {preview && <>
      <p>{recycle ? "You can restore the file from the Windows Recycle Bin."
        : restore ? "Restore this file from the Git index." : selection.hunkId
          ? "Discard the selected hunk. The preview includes the remaining working edits." : "Replace the working file with the version in the Git index."}</p>
      {preview.hunk && <pre className="text-xs">{preview.hunk}</pre>}
      <DiscardComparison comparison={preview} recycle={recycle} />
    </>}
    <div className="file-dialog-footer">
      <Button type="button" variant="outline" disabled={apply.isPending} onClick={close}>Cancel</Button>
      <button type="button" className="fw-btn fw-destructive" disabled={!preview || review.isFetching || Boolean(review.error || apply.error) || apply.isPending}
        onClick={() => { if (preview) apply.mutate(preview.snapshot); }}>{apply.isPending ? "Applying…" : recycle ? "Move reviewed file to Recycle Bin"
          : restore ? "Restore reviewed file" : "Discard reviewed changes"}</button>
    </div>
  </FileDialog>;
}

export function GitBatchDiscardDialog({ checkoutId, paths, close, discarded }: {
  checkoutId: string; paths: readonly string[]; close: () => void; discarded: () => void;
}) {
  const client = useQueryClient();
  const [path, select] = useState("");
  const args = { checkoutId, paths };
  const review = useQuery({
    queryKey: ["git-discard-batch-preview", args], queryFn: () => invoke<BatchPreview>("git_discard_batch_preview", args),
    retry: false, refetchOnMount: "always", refetchOnWindowFocus: false,
  });
  const apply = useMutation({
    mutationFn: (snapshot: string) => invoke<void>("git_discard_batch_changes", { ...args, snapshot }),
    onSuccess: () => {
      const restored = review.data?.files.filter((entry) => entry.before.status === "missing") ?? [];
      if (restored.length) publishCheckoutItemChanges(checkoutId, "copy", restored.map(({ path }) => ({ source: path, destination: path, directory: false })));
      discarded();
    },
    onSettled: () => Promise.all([
      client.invalidateQueries({ queryKey: ["git-review", checkoutId] }),
      client.invalidateQueries({ queryKey: ["git-live-diff", checkoutId] }),
    ]),
  });
  const preview = review.data;
  const comparison = preview?.files.find((entry) => entry.path === path) ?? preview?.files[0];
  return <FileDialog title="Discard selected file changes" description={`${paths.length} tracked files`}
    className="file-comparison-dialog" onClose={() => { if (!apply.isPending) close(); }}>
    {review.isFetching && <p role="status">Preparing the selected files from the Git index…</p>}
    {review.error && <InlineIssue>{String(review.error)}</InlineIssue>}
    {apply.error && <InlineIssue>{String(apply.error)}</InlineIssue>}
    {preview && comparison && <>
      <p>Replace the selected working files with the versions in the Git index.</p>
      <label className="file-field">File to review
        <select value={comparison.path} onChange={(event) => select(event.target.value)}>
          {preview.files.map((file) => <option key={file.path} value={file.path}>{file.path}</option>)}
        </select>
      </label>
      <DiscardComparison key={comparison.path} comparison={comparison} />
    </>}
    <div className="file-dialog-footer">
      <Button type="button" variant="outline" disabled={apply.isPending} onClick={close}>Cancel</Button>
      {(apply.error || review.error) && <Button type="button" variant="outline" disabled={review.isFetching || apply.isPending}
        onClick={() => { apply.reset(); void review.refetch(); }}>Review current changes</Button>}
      <button type="button" className="fw-btn fw-destructive" disabled={!preview || !comparison || review.isFetching || Boolean(review.error || apply.error) || apply.isPending}
        onClick={() => { if (preview) apply.mutate(preview.snapshot); }}>
        {apply.isPending ? "Applying…" : `Discard changes in ${paths.length} files`}
      </button>
    </div>
  </FileDialog>;
}

function DiscardComparison({ comparison, recycle }: { comparison: Comparison; recycle?: boolean }) {
  const { before, after, path } = comparison;
  if (before.status === "bytes" || after.status !== "text" && !recycle) return <dl className="grid grid-cols-2 gap-2 text-xs" aria-label="Discard preview">
    <dt>Current working file</dt><dd><PreviewMetadata file={before} /></dd>
    <dt>Result after confirmation</dt><dd><PreviewMetadata file={after} /></dd>
  </dl>;
  return <div className="file-comparison-body" aria-label="Discard preview">
    <Suspense fallback={<p role="status">Loading the comparison…</p>}>
      <MonacoComparison layout="split" file={{ path, language: "plaintext",
        original: before.status === "text" ? before.content : "", modified: after.status === "text" ? after.content : "" }}
        revisions={{ original: before.status === "missing" ? "File absent" : "Current working file",
          modified: recycle ? "File moved to Recycle Bin" : "Result after confirmation" }} />
    </Suspense>
  </div>;
}

function PreviewMetadata({ file }: { file: PreviewFile }) {
  if (file.status === "missing") return "File absent";
  if (file.status === "text") return `${new TextEncoder().encode(file.content).length} bytes of UTF-8 text`;
  return <>{file.size} bytes. {file.reason} <code className="block break-all">SHA-256 {file.sha256}</code></>;
}
