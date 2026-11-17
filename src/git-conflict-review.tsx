import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import type { GitFileSide } from "@/git-file-versions";
import { FileDialog } from "@/ide-file-workspace";
import { InlineIssue } from "@/tasks-git-ui";

type ConflictStage = { objectId: string; mode: string; file: GitFileSide };
type ConflictSide = "local" | "incoming";
type ConflictReview = {
  path: string;
  revision: { branch: string | null; head: string | null; index: string | null };
  base: ConflictStage | null;
  local: ConflictStage | null;
  incoming: ConflictStage | null;
  deletedSide: ConflictSide | null;
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
type ProposalChoice = { kind: ConflictSide; objectId: string } | { kind: "combined" } | { kind: "deletion"; side: ConflictSide };
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
    onSuccess: async (_result, saved) => {
      setProposal(undefined);
      confirmProposal(false);
      setConfirmed(undefined);
      setVersion("working");
      await Promise.all([
        review.refetch(),
        client.invalidateQueries({ queryKey: ["git-review", checkoutId] }),
        client.invalidateQueries({ queryKey: ["git-live-diff", checkoutId] }),
      ]);
      setNotice(saved.choice.kind === "deletion"
        ? "Working file moved to the Recycle Bin. Review its absence before staging the deletion. To recover the contents, restore the file by name and move it back to the checkout."
        : "Working result saved. Review it to stage the resolution.");
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
  const side = version === "local" || version === "incoming" ? version : undefined;
  const choosesDeletion = Boolean(side && current?.deletedSide === side);
  const chooseLabel = `Use ${side ?? "selected"} ${choosesDeletion ? "deletion" : "version"}…`;
  const canChoose = !proposal && proposalSnapshot && side && (stage?.file.status === "text"
    || choosesDeletion && current?.working.status === "text");
  const removesFile = proposal?.choice.kind === "deletion";
  const stagesDeletion = current?.working.status === "missing" && Boolean(current.deletedSide);
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
    if (!canChoose || !side) return;
    if (choosesDeletion && current?.working.status === "text") {
      startProposal({ kind: "deletion", side }, current.working.content);
    } else if (stage?.file.status === "text") {
      startProposal({ kind: side, objectId: stage.objectId }, stage.file.content);
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
              : <p>{choosesDeletion ? "This side deleted the file." : "File absent in this version."}</p>}
          </div>
        </ContextActions>
      </section>
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      {(current.proposalBlocked ?? current.resolutionBlocked) && <p role="status">{current.proposalBlocked ?? current.resolutionBlocked}</p>}
      {proposal && <section aria-label="Proposed resolution" className="grid gap-2 border-t border-border pt-3">
        {removesFile ? <>
          <p>The working result will be absent. These exact working contents will be moved to the Recycle Bin.</p>
          <pre aria-label="File contents to remove" tabIndex={0}
            className="max-h-52 overflow-auto whitespace-pre-wrap wrap-anywhere rounded border border-border px-2 py-1.5 font-mono text-xs">
            {proposal.text || "Empty file"}
          </pre>
        </> : proposal.choice.kind === "combined" ? <label className="file-field">Proposed working result
          <textarea rows={8} className="resize-y font-mono text-xs" value={proposal.text} disabled={busy}
            onChange={(event) => {
              setProposal({ ...proposal, text: event.currentTarget.value });
              confirmProposal(false);
            }} />
        </label> : <pre aria-label="Proposed working result" tabIndex={0}
          className="max-h-52 overflow-auto whitespace-pre-wrap wrap-anywhere rounded border border-border px-2 py-1.5 font-mono text-xs">
          {proposal.text || "Empty file"}
        </pre>}
        <p className="text-xs text-muted-foreground">{proposal.choice.kind === "deletion"
          ? `Recorded deletion from ${versionLabels[proposal.choice.side]}.`
          : proposal.choice.kind === "combined"
          ? "Combined text uses UTF-8 with LF line endings."
          : `Exact text from ${versionLabels[proposal.choice.kind]}.`} {proposedBytes.toLocaleString()} bytes.</p>
        {tooLarge && <p role="alert">The proposed result exceeds the 2 MiB text limit.</p>}
        {proposalStale && <p role="alert">{removesFile
          ? "The conflict changed. Review the current versions before removing the working file."
          : "The conflict changed. Copy your proposal before cancelling and review the current versions."}</p>}
        <label className="flex items-start gap-2 text-xs">
          <input type="checkbox" checked={proposalConfirmed} disabled={Boolean(proposalStale) || busy || tooLarge || !proposalSnapshot}
            onChange={(event) => confirmProposal(event.currentTarget.checked)} />
          {removesFile ? "I reviewed the working contents and want to move this file to the Recycle Bin."
            : createsFile ? "I reviewed the proposed contents and want to create the working result."
            : "I reviewed the proposed contents and want to replace the working result."}
        </label>
      </section>}
      {stageSnapshot && version === "working" && <label className="flex items-start gap-2 text-xs">
        <input type="checkbox" checked={confirmedSnapshot === current.snapshot} onChange={(event) => {
          setConfirmed(event.currentTarget.checked ? current.snapshot ?? undefined : undefined);
        }} />
        {stagesDeletion ? "I reviewed the absent working file and want to stage its deletion."
          : "I reviewed this working result and want to stage it as the resolution."}
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
          {apply.isPending ? "Saving…" : removesFile ? "Remove working file" : createsFile ? "Create working result" : "Replace working result"}
        </Button>
      </>}
      {current && !proposal && <Button type="button" disabled={!stageSnapshot || version !== "working" || confirmedSnapshot !== stageSnapshot}
        onClick={() => { if (stageSnapshot && version === "working" && confirmedSnapshot === stageSnapshot) resolve.mutate(stageSnapshot); }}>
        {resolve.isPending ? "Staging…" : stagesDeletion ? "Stage reviewed deletion" : "Stage reviewed result"}
      </Button>}
      <Button type="button" variant="ghost" disabled={busy} onClick={close}>Close</Button>
    </div>
  </FileDialog>;
}
