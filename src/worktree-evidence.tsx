import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useContext, useState } from "react";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { decodeArtifactBytes, downloadArtifact } from "@/download-artifact";
import { RunInspectorNavigation } from "@/run-inspector-navigation";

type FileChange = { path: string; sha256: string; contentBase64?: string };
type Snapshot = {
  head: string;
  changes: {
    stagedPatchBase64: string;
    unstagedPatchBase64: string;
    untracked: FileChange[];
  } | null;
};
type Evidence = { kind: "unavailable"; reason: string } | {
  kind: "saved";
  artifact: {
    revision: string;
    start: { starting: Snapshot; worktree: { ownerTaskId: string; branch: string } };
    candidate: Snapshot;
    committedPatchBase64: string;
  };
};
type LiveCheckout = { worktree: { branch: string }; head: string; status: string[] };

function decode(base64: string) {
  return new TextDecoder().decode(decodeArtifactBytes(base64));
}

function Patch({ title, data }: { title: string; data: string }) {
  const [open, setOpen] = useState(false);
  const { copy, feedback, run } = useContextActionFeedback();
  if (!data) return null;
  return (
    <details onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>{title}</summary>
      {open && <>
        <ContextActions label={title} groups={[[
          { label: "Copy saved patch", run: () => copy(decode(data)) },
          { label: "Download saved patch", run: () => void run(async () => {
            downloadArtifact(`${title.toLowerCase().replaceAll(/[^a-z0-9]+/g, "-")}.patch`, new Blob([decodeArtifactBytes(data)], { type: "application/octet-stream" }));
          }) },
        ]]}>
          <pre tabIndex={0} className="max-h-72 min-w-0 flex-1 overflow-auto rounded bg-muted p-2 text-xs">{decode(data)}</pre>
        </ContextActions>
        {feedback && <p role={feedback.failed ? "alert" : "status"} className="text-xs">{feedback.text}</p>}
      </>}
    </details>
  );
}

function SnapshotFile({ file }: { file: FileChange }) {
  const { copy, feedback, run } = useContextActionFeedback();
  const bytes = file.contentBase64;
  const name = file.path.split("/").at(-1) || "captured-file";
  const download = () => void run(async () => {
    if (bytes !== undefined) downloadArtifact(name, new Blob([decodeArtifactBytes(bytes)], { type: "application/octet-stream" }));
  });
  return <li className="break-all">
    <ContextActions label={file.path} groups={[[
      { label: "Copy relative path", run: () => copy(file.path) },
      { label: "Copy saved SHA-256", run: () => copy(file.sha256) },
      ...(bytes !== undefined ? [{ label: "Download captured file", run: download }] : []),
    ]]}>
      <div className="min-w-0 flex-1">
        {bytes === undefined ? file.path : <button type="button" className="text-left underline" onClick={download}>{file.path}</button>}
        <span className="ml-2 text-xs text-muted-foreground" title={`SHA-256 ${file.sha256}`}>{file.sha256.slice(0, 12)}</span>
      </div>
    </ContextActions>
    {feedback && <p role={feedback.failed ? "alert" : "status"} className="text-xs">{feedback.text}</p>}
  </li>;
}

function SnapshotChanges({ snapshot }: { snapshot: Snapshot }) {
  const changes = snapshot.changes;
  if (!changes) return <p>Detailed changes are unavailable.</p>;
  if (!changes.stagedPatchBase64 && !changes.unstagedPatchBase64 && !changes.untracked.length) {
    return <p>Clean index and checkout.</p>;
  }
  return (
    <>
      <Patch title="Staged changes" data={changes.stagedPatchBase64} />
      <Patch title="Unstaged changes" data={changes.unstagedPatchBase64} />
      {changes.untracked.length > 0 && <ul aria-label="Untracked files">
        {changes.untracked.map((file) => <SnapshotFile key={file.path} file={file} />)}
      </ul>}
    </>
  );
}

export function WorktreeEvidence({ projectId, runId, ownerTaskId }: {
  projectId: string; runId: string; ownerTaskId: string | null;
}) {
  const navigation = useContext(RunInspectorNavigation);
  const { copy, feedback } = useContextActionFeedback();
  const [savedOpen, setSavedOpen] = useState(false);
  const [liveOpen, setLiveOpen] = useState(false);
  const saved = useQuery({
    queryKey: ["attempt-worktree-evidence", projectId, runId],
    queryFn: () => invoke<Evidence | null>("attempt_worktree_evidence", { projectId, runId }),
    enabled: savedOpen,
    staleTime: Infinity,
  });
  const evidence = saved.data;
  const artifact = evidence?.kind === "saved" ? evidence.artifact : undefined;
  const owner = artifact?.start.worktree.ownerTaskId ?? ownerTaskId;
  const live = useQuery({
    queryKey: ["live-managed-worktree", projectId, owner],
    queryFn: () => invoke<LiveCheckout | null>("live_managed_worktree", { projectId, ownerTaskId: owner }),
    enabled: liveOpen && Boolean(owner),
  });
  return (
    <section className="mx-auto mt-4 grid max-w-3xl gap-2 rounded-md border border-border p-3 text-sm" aria-label="Worktree evidence">
      {artifact && navigation?.openGitAttempt && <button type="button" className="conv-quiet-button justify-self-start"
        onClick={() => navigation.openGitAttempt?.(runId)}>Open saved diff in Git</button>}
      <details onToggle={(event) => setSavedOpen(event.currentTarget.open)}>
        <summary>Saved candidate for this attempt</summary>
        {savedOpen && saved.isPending && <p role="status">Loading saved evidence…</p>}
        {saved.error && <p role="alert">{String(saved.error)}</p>}
        {saved.isSuccess && !artifact && <p>{evidence?.kind === "unavailable" ? evidence.reason : "Attempt evidence is unavailable."}</p>}
        {artifact && <div className="grid gap-1 pt-2">
          <ContextActions label="saved candidate" groups={[[
            { label: "Copy candidate revision ID", run: () => copy(artifact.revision) },
            { label: "Copy saved HEAD", run: () => copy(artifact.candidate.head) },
            { label: "Copy starting HEAD", run: () => copy(artifact.start.starting.head) },
            ...(navigation?.openGitAttempt ? [{ label: "Open saved diff in Git", run: () => navigation.openGitAttempt?.(runId) }] : []),
          ]]}><p>Candidate <code title={artifact.revision}>{artifact.revision.slice(0, 12)}</code></p></ContextActions>
          {feedback && <p role={feedback.failed ? "alert" : "status"} className="text-xs">{feedback.text}</p>}
          <p>Saved HEAD <code>{artifact.candidate.head}</code></p>
          <Patch title="Changes committed during the attempt" data={artifact.committedPatchBase64} />
          <SnapshotChanges snapshot={artifact.candidate} />
          <details>
            <summary>Starting Git state</summary>
            <p>Starting HEAD <code>{artifact.start.starting.head}</code></p>
            <SnapshotChanges snapshot={artifact.start.starting} />
          </details>
        </div>}
      </details>
      {owner && <details onToggle={(event) => setLiveOpen(event.currentTarget.open)}>
        <summary>Live checkout status</summary>
        {liveOpen && live.isPending && <p role="status">Checking the live checkout…</p>}
        {live.error && <p role="alert">{String(live.error)}</p>}
        {live.isSuccess && !live.data && <p>Live checkout unavailable.</p>}
        {live.data && <>
          <p>{live.data.worktree.branch} · HEAD <code>{live.data.head}</code></p>
          {live.data.status.length > 0 ? (
            <pre className="max-h-56 overflow-auto rounded bg-muted p-2 text-xs">{live.data.status.join("\n")}</pre>
          ) : <p>Clean index and checkout.</p>}
        </>}
      </details>}
    </section>
  );
}
