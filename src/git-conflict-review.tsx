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
  proposalBlocked: string | null;
};
const versions = ["base", "local", "incoming", "working"] as const;
type Version = typeof versions[number];
const versionLabels: Record<Version, string> = {
  base: "Base · stage 1",
  local: "Local · stage 2",
  incoming: "Incoming · stage 3",
  working: "Working result",
};
type ProposalChoice = { kind: "local" | "incoming"; objectId: string } | { kind: "combined" };
type Proposal = { expected: string; choice: ProposalChoice; text: string };
const maxProposalBytes = 2 * 1024 * 1024;

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
  const [proposal, setProposal] = useState<Proposal>();
  const [proposalConfirmed, confirmProposal] = useState(false);
  const [notice, setNotice] = useState("");
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
  const apply = useMutation({
    mutationFn: ({ expected, choice, text }: Proposal) => invoke<void>("git_apply_conflict_resolution", {
      checkoutId, path, expected, proposal: choice.kind === "combined" ? { kind: "combined", content: text } : choice,
    }),
    onSuccess: async () => {
      setProposal(undefined);
      confirmProposal(false);
      setConfirmed(undefined);
      setVersion("working");
      await Promise.all([
        review.refetch(),
        client.invalidateQueries({ queryKey: ["git-review", checkoutId] }),
        client.invalidateQueries({ queryKey: ["git-live-diff", checkoutId] }),
      ]);
      setNotice("Working result saved. Review it to stage the resolution.");
    },
  });
  const current = review.data;
  const stage = version === "working" ? undefined : current?.[version];
  const file = version === "working" ? current?.working : stage?.file;
  const workingFile = current?.working.status === "text" ? openFile : undefined;
  const busy = resolve.isPending || apply.isPending;
  const fresh = !review.isFetching && !review.error && !busy;
  const stageSnapshot = current && !current.resolutionBlocked && fresh && !proposal
    ? current.snapshot : null;
  const proposalSnapshot = current && !current.proposalBlocked && fresh ? current.snapshot : null;
  const chooseLabel = version === "local" ? "Use local version…" : "Use incoming version…";
  const canChoose = !proposal && proposalSnapshot && (version === "local" || version === "incoming") && stage?.file.status === "text";
  const proposalStale = proposal && current?.snapshot !== proposal.expected;
  const proposedBytes = proposal ? new TextEncoder().encode(proposal.text).byteLength : 0;
  const tooLarge = proposedBytes > maxProposalBytes;
  const canApply = !proposalStale && proposalSnapshot && proposalConfirmed && !tooLarge;
  const createsFile = current?.working.status === "missing";
  function openWorkingResult() {
    if (!workingFile) return;
    close();
    workingFile(path);
  }
  function refresh() {
    setConfirmed(undefined);
    confirmProposal(false);
    setNotice("");
    resolve.reset();
    apply.reset();
    void review.refetch();
  }
  function startProposal(choice: ProposalChoice, text: string) {
    if (!proposalSnapshot) return;
    apply.reset();
    setNotice("");
    setConfirmed(undefined);
    confirmProposal(false);
    setProposal({ expected: proposalSnapshot, choice, text });
  }
  function chooseVersion() {
    if (canChoose && stage?.file.status === "text" && (version === "local" || version === "incoming")) {
      startProposal({ kind: version, objectId: stage.objectId }, stage.file.content);
    }
  }
  function combine() {
    const entry = file?.status === "text" ? file : current?.working;
    const start = entry?.status === "text" ? entry.content : "";
    startProposal({ kind: "combined" }, start.replace(/\r\n?/g, "\n"));
  }
  function cancelProposal() {
    setProposal(undefined);
    confirmProposal(false);
    apply.reset();
  }
  return <FileDialog title="Review Git conflict" description={`${path} · ${checkoutRoot}`}
    className="git-conflict-dialog" onClose={() => { if (!busy) close(); }}>
    {review.isFetching && <p role="status">Reading Git conflict stages…</p>}
    {notice && <p role="status">{notice}</p>}
    {(review.error || resolve.error || apply.error) && <InlineIssue>{String(review.error || resolve.error || apply.error)}</InlineIssue>}
    {current && <>
      <div className="flex flex-wrap gap-1" role="group" aria-label="Conflict versions">
        {versions.map((entry) => <Button key={entry} type="button" variant={entry === version ? "default" : "outline"}
          disabled={busy} aria-pressed={entry === version} onClick={() => { setVersion(entry); setConfirmed(undefined); }}>
          {versionLabels[entry]}
        </Button>)}
      </div>
      <section aria-label={versionLabels[version]} className="grid min-w-0 gap-1">
        <ContextActions label={versionLabels[version]} groups={[[
          ...(file?.status === "text" ? [{ label: "Copy version text", run: () => copy(file.content) }] : []),
          ...(stage ? [{ label: "Copy Git object ID", run: () => copy(stage.objectId) }] : []),
          ...(version === "working" && workingFile && !proposal ? [{ label: "Open working result in editor", run: openWorkingResult }] : []),
          ...(canChoose ? [{ label: chooseLabel, run: chooseVersion }] : []),
          ...(proposalSnapshot && !proposal ? [{ label: "Combine manually…", run: combine }] : []),
        ]]}>
          <div className="min-w-0 flex-1 rounded border border-border px-2 py-1.5">
            {stage && <p className="mb-1 wrap-anywhere font-mono text-xs text-muted-foreground">{stage.mode} · {stage.objectId}</p>}
            {file?.status === "text" ? <pre aria-label={`${versionLabels[version]} text`} tabIndex={0}
              className={`${proposal ? "max-h-36" : "max-h-64"} overflow-auto whitespace-pre-wrap wrap-anywhere font-mono text-xs`}>{file.content || "Empty file"}</pre>
              : file?.status === "unavailable" ? <p>{file.reason}</p>
              : <p>File absent in this version.</p>}
          </div>
        </ContextActions>
      </section>
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      {(current.proposalBlocked ?? current.resolutionBlocked) && <p role="status">{current.proposalBlocked ?? current.resolutionBlocked}</p>}
      {proposal && <section aria-label="Proposed resolution" className="grid gap-2 border-t border-border pt-3">
        {proposal.choice.kind === "combined" ? <label className="file-field">Proposed working result
          <textarea rows={8} className="resize-y font-mono text-xs" value={proposal.text} disabled={busy}
            onChange={(event) => {
              setProposal({ ...proposal, text: event.currentTarget.value });
              confirmProposal(false);
            }} />
        </label> : <pre aria-label="Proposed working result" tabIndex={0}
          className="max-h-52 overflow-auto whitespace-pre-wrap wrap-anywhere rounded border border-border px-2 py-1.5 font-mono text-xs">
          {proposal.text || "Empty file"}
        </pre>}
        <p className="text-xs text-muted-foreground">{proposal.choice.kind === "combined"
          ? "Combined text uses UTF-8 with LF line endings."
          : `Exact text from ${versionLabels[proposal.choice.kind]}.`} {proposedBytes.toLocaleString()} bytes.</p>
        {tooLarge && <p role="alert">The proposed result exceeds the 2 MiB text limit.</p>}
        {proposalStale && <p role="alert">The conflict changed. Copy your proposal before cancelling and review the current versions.</p>}
        <label className="flex items-start gap-2 text-xs">
          <input type="checkbox" checked={proposalConfirmed} disabled={Boolean(proposalStale) || busy || tooLarge || !proposalSnapshot}
            onChange={(event) => confirmProposal(event.currentTarget.checked)} />
          {createsFile ? "I reviewed the proposed contents and want to create the working result."
            : "I reviewed the proposed contents and want to replace the working result."}
        </label>
      </section>}
      {stageSnapshot && version === "working" && <label className="flex items-start gap-2 text-xs">
        <input type="checkbox" checked={confirmedSnapshot === current.snapshot} onChange={(event) => {
          setConfirmed(event.currentTarget.checked ? current.snapshot ?? undefined : undefined);
        }} />
        I reviewed this working result and want to stage it as the resolution.
      </label>}
    </>}
    <div className="file-dialog-footer flex-wrap">
      <Button type="button" variant="outline" disabled={review.isFetching || busy} onClick={refresh}>Refresh review</Button>
      {workingFile && !proposal && <Button type="button" variant="outline" disabled={busy} onClick={openWorkingResult}>Open working result in editor</Button>}
      {canChoose && <Button type="button" variant="outline" onClick={chooseVersion}>{chooseLabel}</Button>}
      {proposalSnapshot && !proposal && <Button type="button" variant="outline" onClick={combine}>Combine manually…</Button>}
      {proposal && <>
        <Button type="button" variant="ghost" disabled={busy} onClick={cancelProposal}>Cancel proposal</Button>
        <Button type="button" disabled={!canApply} onClick={() => { if (canApply) apply.mutate(proposal); }}>
          {apply.isPending ? "Saving…" : createsFile ? "Create working result" : "Replace working result"}
        </Button>
      </>}
      {current && !proposal && <Button type="button" disabled={!stageSnapshot || version !== "working" || confirmedSnapshot !== stageSnapshot}
        onClick={() => { if (stageSnapshot && version === "working" && confirmedSnapshot === stageSnapshot) resolve.mutate(stageSnapshot); }}>
        {resolve.isPending ? "Staging…" : "Stage reviewed result"}
      </Button>}
      <Button type="button" variant="ghost" disabled={busy} onClick={close}>Close</Button>
    </div>
  </FileDialog>;
}
