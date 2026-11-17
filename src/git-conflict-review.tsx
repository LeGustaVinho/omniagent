import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import type { GitFileSide } from "@/git-file-versions";
import { FileDialog } from "@/ide-file-workspace";
import { InlineIssue } from "@/tasks-git-ui";

type ConflictStage = { objectId: string; mode: string; file: GitFileSide };
type ConflictReview = {
  path: string;
  revision: { branch: string | null; head: string | null; index: string | null };
  base: ConflictStage | null;
  local: ConflictStage | null;
  incoming: ConflictStage | null;
  working: GitFileSide;
  snapshot: string | null;
  resolutionBlocked: string | null;
};
const versions = ["base", "local", "incoming", "working"] as const;
type Version = typeof versions[number];
const versionLabels: Record<Version, string> = {
  base: "Base · stage 1",
  local: "Local · stage 2",
  incoming: "Incoming · stage 3",
  working: "Working result",
};

export function GitConflictReview({ checkoutId, checkoutRoot, path, openFile, close }: {
  checkoutId: string;
  checkoutRoot: string;
  path: string;
  openFile?: (path: string) => void;
  close: () => void;
}) {
  const client = useQueryClient();
  const [version, setVersion] = useState<Version>("working");
  const [confirmedSnapshot, setConfirmed] = useState<string>();
  const { copy, feedback } = useContextActionFeedback();
  const key = ["git-conflict", checkoutId, path];
  const review = useQuery({
    queryKey: key,
    queryFn: () => invoke<ConflictReview>("git_conflict_review", { checkoutId, path }),
    retry: false,
    refetchOnMount: "always",
    refetchOnWindowFocus: false,
  });
  const resolve = useMutation({
    mutationFn: (expected: string) => invoke<void>("git_mark_conflict_resolved", { checkoutId, path, expected }),
    onSuccess: async () => {
      await Promise.all([
        client.invalidateQueries({ queryKey: ["git-review", checkoutId] }),
        client.invalidateQueries({ queryKey: ["git-live-diff", checkoutId] }),
        client.invalidateQueries({ queryKey: key, refetchType: "none" }),
      ]);
      close();
    },
  });
  const current = review.data;
  const stage = version === "working" ? undefined : current?.[version];
  const file = version === "working" ? current?.working : stage?.file;
  const workingFile = current?.working.status === "text" ? openFile : undefined;
  const stageSnapshot = current && !current.resolutionBlocked && !review.isFetching && !review.error && !resolve.isPending
    ? current.snapshot : null;
  function openWorkingResult() {
    if (!workingFile) return;
    close();
    workingFile(path);
  }
  function refresh() {
    setConfirmed(undefined);
    resolve.reset();
    void review.refetch();
  }
  return <FileDialog title="Review Git conflict" description={`${path} · ${checkoutRoot}`}
    className="git-conflict-dialog" onClose={() => { if (!resolve.isPending) close(); }}>
    {review.isFetching && <p role="status">Reading Git conflict stages…</p>}
    {(review.error || resolve.error) && <InlineIssue>{String(review.error || resolve.error)}</InlineIssue>}
    {current && <>
      <div className="flex flex-wrap gap-1" role="group" aria-label="Conflict versions">
        {versions.map((entry) => <Button key={entry} type="button" variant={entry === version ? "default" : "outline"}
          disabled={resolve.isPending} aria-pressed={entry === version} onClick={() => { setVersion(entry); setConfirmed(undefined); }}>
          {versionLabels[entry]}
        </Button>)}
      </div>
      <section aria-label={versionLabels[version]} className="grid min-w-0 gap-1">
        <ContextActions label={versionLabels[version]} groups={[[
          ...(file?.status === "text" ? [{ label: "Copy version text", run: () => copy(file.content) }] : []),
          ...(stage ? [{ label: "Copy Git object ID", run: () => copy(stage.objectId) }] : []),
          ...(version === "working" && workingFile ? [{ label: "Open working result in editor", run: openWorkingResult }] : []),
        ]]}>
          <div className="min-w-0 flex-1 rounded border border-border px-2 py-1.5">
            {stage && <p className="mb-1 wrap-anywhere font-mono text-xs text-muted-foreground">{stage.mode} · {stage.objectId}</p>}
            {file?.status === "text" ? <pre aria-label={`${versionLabels[version]} text`} tabIndex={0}
              className="max-h-64 overflow-auto whitespace-pre-wrap wrap-anywhere font-mono text-xs">{file.content || "Empty file"}</pre>
              : file?.status === "unavailable" ? <p>{file.reason}</p>
              : <p>File absent in this version.</p>}
          </div>
        </ContextActions>
      </section>
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      {current.resolutionBlocked && <p role="status">{current.resolutionBlocked}</p>}
      {stageSnapshot && version === "working" && <label className="flex items-start gap-2 text-xs">
        <input type="checkbox" checked={confirmedSnapshot === current.snapshot} onChange={(event) => {
          setConfirmed(event.currentTarget.checked ? current.snapshot ?? undefined : undefined);
        }} />
        I reviewed this working result and want to stage it as the resolution.
      </label>}
    </>}
    <div className="file-dialog-footer flex-wrap">
      <Button type="button" variant="outline" disabled={review.isFetching || resolve.isPending} onClick={refresh}>Refresh review</Button>
      {workingFile && <Button type="button" variant="outline" disabled={resolve.isPending} onClick={openWorkingResult}>Open working result in editor</Button>}
      {current && <Button type="button" disabled={!stageSnapshot || version !== "working" || confirmedSnapshot !== stageSnapshot}
        onClick={() => { if (stageSnapshot && version === "working" && confirmedSnapshot === stageSnapshot) resolve.mutate(stageSnapshot); }}>
        {resolve.isPending ? "Staging…" : "Stage reviewed result"}
      </Button>}
      <Button type="button" variant="ghost" disabled={resolve.isPending} onClick={close}>Close</Button>
    </div>
  </FileDialog>;
}
