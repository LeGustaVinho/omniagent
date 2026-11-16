import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { ConversationHistoryDeletion } from "@/conversation-history-deletion";
import { loadMonaco, monacoTheme } from "@/ide-monaco";
import type { WorkspaceProps } from "@/workbench-contract";

type SavedAttempt = { runId: string; checkoutId: string; label: string; conversationId?: string; historyDeleted?: boolean };
type Untracked = { path: string; sha256: string; contentBase64?: string };
type ChangeArtifact = {
  revision: string;
  start: { worktree: { branch: string }; starting: { head: string } };
  candidate: { head: string; changes?: { stagedPatchBase64: string; unstagedPatchBase64: string; untracked: Untracked[] } | null };
  committedPatchBase64: string;
};
type Evidence = { kind: "saved"; artifact: ChangeArtifact } | { kind: "unavailable"; reason: string };
type Part = { id: string; label: string; encoded?: string; language: "diff" | "plaintext"; sha?: string };

export function SavedGitDiffs({ projectId, target, openActivity }: { projectId: string; target?: string; openActivity: WorkspaceProps["openActivity"] }) {
  const requested = target?.startsWith("attempt:") ? target.slice("attempt:".length) : "";
  const [selected, select] = useState(requested);
  useEffect(() => { if (requested) select(requested); }, [requested]);
  const attempts = useQuery({ queryKey: ["git-saved-attempts", projectId], queryFn: () => invoke<SavedAttempt[]>("git_saved_attempts", { projectId }), retry: false });
  const runId = selected || attempts.data?.[0]?.runId || "";
  const evidence = useQuery({
    queryKey: ["git-attempt-diff", projectId, runId],
    queryFn: () => invoke<Evidence | null>("attempt_worktree_evidence", { projectId, runId }), enabled: !!runId, retry: false,
  });
  const attemptOptions = attempts.data ?? [];
  const attempt = attemptOptions.find((item) => item.runId === runId);
  return <div className="delivery-main flex min-w-0 flex-col gap-3 p-4">
    <div className="flex flex-wrap items-end gap-2">
      <label className="delivery-field flex-1">Recorded attempt
        <select className="delivery-filter" value={runId} onChange={(event) => select(event.target.value)} disabled={!runId}>
          {!runId && <option value="">No saved attempts</option>}
          {runId && !attemptOptions.some(({ runId: id }) => id === runId) && <option value={runId}>{runId}</option>}
          {attemptOptions.map((attempt) => <option key={attempt.runId} value={attempt.runId}>{attempt.label} · {attempt.runId.slice(0, 8)}</option>)}
        </select>
      </label>
      {runId && <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" onClick={() => openActivity(runId)}>View attempt</Button>}
    </div>
    {(attempts.error || evidence.error) && <p role="alert">{String(attempts.error ?? evidence.error)}</p>}
    {(attempts.isPending || (runId && evidence.isPending)) && <p role="status">Loading recorded Git evidence…</p>}
    {evidence.data === null && <p>Saved Git evidence is unavailable for this attempt.</p>}
    {evidence.data?.kind === "unavailable" && <p>{evidence.data.reason}</p>}
    {evidence.data?.kind === "saved" && <RecordedArtifact key={`evidence:${runId}`} artifact={evidence.data.artifact} />}
    {attempt?.conversationId && (evidence.data?.kind === "saved" || attempt.historyDeleted) && <details key={`delete:${runId}`}>
      <summary className="cursor-pointer text-sm">Delete recorded diff</summary>
      <ConversationHistoryDeletion conversationId={attempt.conversationId}
        attemptDiffs={[{ runId, label: attempt.label, deleted: attempt.historyDeleted }]} />
    </details>}
  </div>;
}

function RecordedArtifact({ artifact }: { artifact: ChangeArtifact }) {
  const changes = artifact.candidate.changes;
  const allParts: Part[] = [
    { id: "committed", label: "Commits during attempt", encoded: artifact.committedPatchBase64, language: "diff" },
    { id: "staged", label: "Captured index", encoded: changes?.stagedPatchBase64, language: "diff" },
    { id: "working", label: "Captured working changes", encoded: changes?.unstagedPatchBase64, language: "diff" },
    ...(changes?.untracked ?? []).map((entry): Part => ({ id: `untracked:${entry.path}`, label: entry.path, encoded: entry.contentBase64, language: "plaintext", sha: entry.sha256 })),
  ];
  const parts = allParts.filter((entry) => entry.encoded || entry.sha);
  const [choice, select] = useState("");
  const part = parts.find((entry) => entry.id === choice) ?? parts[0];
  const text = decode(part?.encoded);
  return <>
    <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 break-all font-mono text-xs" aria-label="Recorded comparison identity">
      <dt>Branch</dt><dd>{artifact.start.worktree.branch}</dd>
      <dt>Starting HEAD</dt><dd>{artifact.start.starting.head}</dd>
      <dt>Captured HEAD</dt><dd>{artifact.candidate.head}</dd>
      <dt>Capture revision</dt><dd>{artifact.revision}</dd>
    </dl>
    {part ? <>
      <label className="delivery-field">Recorded diff source<select className="delivery-filter" value={part.id} onChange={(event) => select(event.target.value)}>
        {parts.map((entry) => <option key={entry.id} value={entry.id}>{entry.label}</option>)}
      </select></label>
      {part.sha && <p className="break-all font-mono text-xs">SHA-256 {part.sha}</p>}
      {text === undefined ? <p>Recorded text is unavailable for this file.</p> : <RecordedMonaco key={part.id} label={part.label} text={text} language={part.language} />}
    </> : <p>No patch was recorded for this attempt.</p>}
  </>;
}

function decode(encoded?: string) {
  if (encoded === undefined) return undefined;
  try {
    const bytes = Uint8Array.from(atob(encoded), (byte) => byte.charCodeAt(0));
    const value = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return value.includes("\0") ? undefined : value;
  } catch { return undefined; }
}

function RecordedMonaco({ label, text, language }: { label: string; text: string; language: string }) {
  const host = useRef<HTMLDivElement>(null);
  const [error, setError] = useState(false);
  useEffect(() => {
    let close: (() => void) | undefined;
    let active = true;
    void loadMonaco().then((monaco) => {
      if (!active || !host.current) return;
      const model = monaco.editor.createModel(text, language);
      const editor = monaco.editor.create(host.current, {
        model, ariaLabel: label, readOnly: true, automaticLayout: true, theme: monacoTheme(),
        minimap: { enabled: false }, scrollBeyondLastLine: false,
      });
      close = () => { editor.dispose(); model.dispose(); };
    }).catch(() => { if (active) setError(true); });
    return () => { active = false; close?.(); };
  }, [label, language, text]);
  return error ? <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-all text-xs" aria-label={label}>{text}</pre>
    : <div ref={host} className="min-h-64 flex-1" aria-label={label} />;
}
