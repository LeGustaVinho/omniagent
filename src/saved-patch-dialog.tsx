import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { lazy, Suspense, useState } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { Button } from "@/components/ui/button";
import { FileDialog } from "@/ide-file-workspace";
import { InlineIssue } from "@/tasks-git-ui";

export type SavedPatchPart = "committed" | "staged" | "working";
export type SavedPatchSelection = { projectId: string; runId: string; captureRevision: string; part: SavedPatchPart };

type FileVersion = { status: "missing" } | { status: "text"; content: string } | { status: "bytes"; size: number; sha256: string };
type Preview = { snapshot: string; destination: string; revision: { branch: string | null; head: string | null; index: string | null };
  files: { path: string; before: FileVersion; after: FileVersion }[] };
const labels = { committed: "Commits during attempt", staged: "Captured index", working: "Captured working changes" };

const MonacoComparison = lazy(() => import("@/git-diff-view").then(({ MonacoComparison }) => ({ default: MonacoComparison })));

export function SavedPatchDialog({ selection, close, applied }: {
  selection: SavedPatchSelection; close: () => void; applied?: (destination: string) => void;
}) {
  const client = useQueryClient();
  const [checkoutId, setCheckout] = useState("");
  const [path, setPath] = useState("");
  const request = { ...selection, checkoutId };
  const checkouts = useQuery({ queryKey: ["registered-checkouts"], queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts"), retry: false });
  const destinations = (checkouts.data ?? []).filter((checkout) => checkout.projectId === selection.projectId);
  const review = useMutation({ mutationFn: () => invoke<Preview>("preview_saved_git_patch", { request }) });
  const apply = useMutation({
    mutationFn: (snapshot: string) => invoke<void>("apply_saved_git_patch", { request, snapshot }),
    onSuccess: () => { applied?.(review.data?.destination ?? ""); close(); },
    onSettled: () => Promise.all([
      client.invalidateQueries({ queryKey: ["git-review", checkoutId] }),
      client.invalidateQueries({ queryKey: ["git-live-diff", checkoutId] }),
      client.invalidateQueries({ queryKey: ["checkout-folders", checkoutId] }),
    ]),
  });
  const busy = review.isPending || apply.isPending;
  const preview = review.data;
  const file = preview?.files.find((candidate) => candidate.path === path) ?? preview?.files[0];
  function pick(value: string) {
    setCheckout(value);
    setPath("");
    review.reset();
    apply.reset();
  }
  return <FileDialog title="Apply saved patch" description={`${labels[selection.part]} · ${selection.captureRevision}`}
    className="file-comparison-dialog" onClose={() => { if (!apply.isPending) close(); }}>
    <div className="flex flex-wrap items-end gap-2">
      <label className="file-field min-w-0 flex-1">Destination worktree
        <select value={checkoutId} onChange={(event) => pick(event.target.value)} disabled={busy || checkouts.isPending}>
          <option value="">Select a registered worktree</option>
          {destinations.map((checkout) => <option key={checkout.checkoutId} value={checkout.checkoutId}>{checkout.checkoutRoot}</option>)}
        </select>
      </label>
      <Button type="button" variant="outline" disabled={!destinations.some((checkout) => checkout.checkoutId === checkoutId) || busy}
        onClick={() => { apply.reset(); setPath(""); review.mutate(); }}>{review.isPending ? "Preparing…" : "Review patch"}</Button>
    </div>
    {checkouts.error && <InlineIssue>{String(checkouts.error)}</InlineIssue>}
    {review.error && <InlineIssue>{String(review.error)}</InlineIssue>}
    {apply.error && <InlineIssue>{String(apply.error)}</InlineIssue>}
    {checkouts.isSuccess && destinations.length === 0 && <p>No registered worktrees are available in this project.</p>}
    {preview && file && <>
      <p className="break-all text-sm">{preview.destination} · <code>{preview.revision.head ?? "Unborn HEAD"}</code></p>
      <label className="file-field">Affected file
        <select value={file.path} onChange={(event) => setPath(event.target.value)}>
          {preview.files.map((entry) => <option key={entry.path} value={entry.path}>{entry.path}</option>)}
        </select>
      </label>
      <p className="text-sm">Apply the reviewed contents as working tree changes.</p>
      {file.before.status !== "bytes" && file.after.status !== "bytes" ? <div className="file-comparison-body" aria-label="Saved patch preview">
        <Suspense fallback={<p role="status">Loading the comparison…</p>}>
          <MonacoComparison key={file.path} layout="split" file={{ path: file.path, language: "plaintext",
            original: file.before.status === "text" ? file.before.content : "", modified: file.after.status === "text" ? file.after.content : "" }}
            revisions={{ original: file.before.status === "missing" ? "File absent" : "Current working file",
              modified: file.after.status === "missing" ? "File removed" : "Result after confirmation" }} />
        </Suspense>
      </div> : <dl className="grid grid-cols-2 gap-2 text-xs" aria-label="Saved patch preview">
        <dt>Current working file</dt><dd><VersionMetadata value={file.before} /></dd>
        <dt>Result after confirmation</dt><dd><VersionMetadata value={file.after} /></dd>
      </dl>}
    </>}
    <div className="file-dialog-footer">
      <Button type="button" variant="outline" disabled={apply.isPending} onClick={close}>Cancel</Button>
      <Button type="button" disabled={!preview || !file || busy || Boolean(apply.error)} onClick={() => { if (preview) apply.mutate(preview.snapshot); }}>
        {apply.isPending ? "Applying…" : "Apply reviewed patch"}
      </Button>
    </div>
  </FileDialog>;
}

function VersionMetadata({ value }: { value: FileVersion }) {
  if (value.status === "missing") return "File absent";
  if (value.status === "text") return `${new TextEncoder().encode(value.content).length} bytes of UTF-8 text`;
  return <>{value.size} bytes <code className="block break-all">SHA-256 {value.sha256}</code></>;
}
