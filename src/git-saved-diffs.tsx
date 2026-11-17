import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import type { editor as MonacoEditor } from "monaco-editor";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { ConversationHistoryDeletion } from "@/conversation-history-deletion";
import { GitDiffFrame } from "@/git-diff-view";
import { loadMonaco, monacoTheme } from "@/ide-monaco";
import { InlineIssue } from "@/tasks-git-ui";
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

type SavedGitDiffsProps = {
  projectId: string; target?: string; openActivity: WorkspaceProps["openActivity"]; onClose?: () => void;
};

export function SavedGitDiffs(props: SavedGitDiffsProps) {
  return <RecordedAttempts key={`${props.projectId}:${props.target ?? ""}`} {...props} />;
}

function RecordedAttempts({ projectId, target, openActivity, onClose }: SavedGitDiffsProps) {
  const requested = target?.startsWith("attempt:") ? target.slice("attempt:".length) : undefined;
  const [selected, select] = useState(requested);
  const attempts = useQuery({ queryKey: ["git-saved-attempts", projectId], queryFn: () => invoke<SavedAttempt[]>("git_saved_attempts", { projectId }), retry: false });
  const runId = selected ?? attempts.data?.[0]?.runId ?? "";
  const evidence = useQuery({
    queryKey: ["git-attempt-diff", projectId, runId],
    queryFn: () => invoke<Evidence | null>("attempt_worktree_evidence", { projectId, runId }), enabled: !!runId, retry: false,
  });
  const attemptOptions = attempts.data ?? [];
  const attempt = attemptOptions.find((item) => item.runId === runId);
  const artifact = !evidence.error && !attempt?.historyDeleted && evidence.data?.kind === "saved" ? evidence.data.artifact : undefined;
  return <GitDiffFrame source="saved" title="Saved diff" onClose={onClose}>
    <div className="delivery-toolbar delivery-filters items-end">
      <label className="delivery-field min-w-0 flex-1">Recorded attempt
        <select className="delivery-filter" value={runId} onChange={(event) => select(event.target.value)} disabled={!runId}>
          {!runId && <option value="">Select an attempt</option>}
          {runId && !attemptOptions.some(({ runId: id }) => id === runId) && <option value={runId}>{runId}</option>}
          {attemptOptions.map((attempt) => <option key={attempt.runId} value={attempt.runId}>{attempt.label} · {attempt.runId.slice(0, 8)}</option>)}
        </select>
      </label>
      {(attempt || evidence.data) && <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs"
        onClick={() => attempt?.conversationId ? openActivity(runId, attempt.conversationId) : openActivity(runId)}>View attempt</Button>}
    </div>
    {artifact ? <RecordedArtifact key={`${runId}:${artifact.revision}`} artifact={artifact} />
      : <div className="delivery-empty">
        {(attempts.error || evidence.error) && <InlineIssue>{String(attempts.error ?? evidence.error)}</InlineIssue>}
        {((!runId && attempts.isPending) || (runId && evidence.isPending)) && <p role="status">Loading recorded Git evidence…</p>}
        {!runId && !attempts.isPending && !attempts.error && <h3>No saved attempts</h3>}
        {evidence.data?.kind === "unavailable" ? <p>{evidence.data.reason}</p>
          : attempt?.historyDeleted ? <p>The recorded diff was removed from history.</p>
          : evidence.data === null && <p>Saved Git evidence is unavailable for this attempt.</p>}
      </div>}
    {attempt?.conversationId && (artifact || attempt.historyDeleted) && <details className="shrink-0 border-t border-border px-3 py-2" key={`delete:${runId}`}>
      <summary className="cursor-pointer text-sm">Delete recorded diff</summary>
      <div className="max-h-60 overflow-auto pt-2">
        <ConversationHistoryDeletion conversationId={attempt.conversationId}
          attemptDiffs={[{ runId, label: attempt.label, deleted: attempt.historyDeleted }]} />
      </div>
    </details>}
  </GitDiffFrame>;
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
  return <div className="delivery-body">
    <div className="delivery-main flex min-w-0 flex-col">
      {part ? <>
        <div className="delivery-toolbar delivery-filters">
          <label className="delivery-field min-w-0">Recorded diff source
            <select className="delivery-filter" value={part.id} onChange={(event) => select(event.target.value)}>
              {parts.map((entry) => <option key={entry.id} value={entry.id}>{entry.label}</option>)}
            </select>
          </label>
          {part.sha && <p className="wrap-anywhere font-mono text-xs">SHA-256 {part.sha}</p>}
        </div>
        {text === undefined ? <p className="p-3 text-sm">Recorded text is unavailable for this file.</p>
          : <RecordedMonaco key={part.id} label={part.label} text={text} language={part.language} />}
      </> : <p className="p-3 text-sm">No patch was recorded for this attempt.</p>}
    </div>
    <aside className="delivery-detail" aria-label="Recorded comparison identity">
      <div className="delivery-detail-content grid content-start gap-3 p-3.5">
        <h3 className="text-sm font-semibold">Capture details</h3>
        <dl className="grid gap-2 text-xs">
          {([
            ["Branch", artifact.start.worktree.branch],
            ["Starting HEAD", artifact.start.starting.head],
            ["Captured HEAD", artifact.candidate.head],
            ["Capture revision", artifact.revision],
          ] as const).map(([label, value]) => <div key={label}>
            <dt className="text-muted-foreground">{label}</dt><dd className="wrap-anywhere font-mono">{value}</dd>
          </div>)}
        </dl>
      </div>
    </aside>
  </div>;
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
    let active = true;
    let model: MonacoEditor.ITextModel | undefined;
    let editor: MonacoEditor.IStandaloneCodeEditor | undefined;
    let observer: MutationObserver | undefined;
    void loadMonaco().then((monaco) => {
      if (!active || !host.current) return;
      const theme = () => monaco.editor.setTheme(monacoTheme());
      model = monaco.editor.createModel(text, language);
      editor = monaco.editor.create(host.current, {
        model, ariaLabel: label, readOnly: true, automaticLayout: true,
        minimap: { enabled: false }, scrollBeyondLastLine: false,
      });
      theme();
      observer = new MutationObserver(theme);
      observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-app-theme", "data-app-motion", "style", "class"] });
    }).catch(() => { if (active) setError(true); });
    return () => {
      active = false;
      observer?.disconnect();
      editor?.dispose();
      model?.dispose();
    };
  }, [label, language, text]);
  return error ? <pre className="min-h-40 flex-1 overflow-auto whitespace-pre-wrap wrap-anywhere p-3 text-xs" aria-label={label}>{text}</pre>
    : <div ref={host} className="min-h-45 flex-1" aria-label={label} />;
}
