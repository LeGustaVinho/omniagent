import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import type { editor as MonacoEditor } from "monaco-editor";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { ConversationHistoryDeletion } from "@/conversation-history-deletion";
import { decodeArtifactBytes, downloadArtifact } from "@/download-artifact";
import { GitDiffFrame } from "@/git-diff-view";
import { fileName } from "@/ide-knowledge-data";
import { loadMonaco, monacoTheme } from "@/ide-monaco";
import { SavedAttemptComparison, type AttemptChoice } from "@/saved-attempt-comparison";
import { SavedPatchDialog, type SavedPatchPart } from "@/saved-patch-dialog";
import { DeliveryDetailPanel, InlineIssue } from "@/tasks-git-ui";
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
type RecordedAction = { label: string; run: () => void };

const patchLabels: Record<SavedPatchPart, string> = {
  committed: "Commits during attempt", staged: "Captured index", working: "Captured working changes",
};

function capturedPatch(artifact: ChangeArtifact, part: SavedPatchPart) {
  if (part === "committed") return artifact.committedPatchBase64;
  return part === "staged" ? artifact.candidate.changes?.stagedPatchBase64 : artifact.candidate.changes?.unstagedPatchBase64;
}

const captureDetails = (artifact: ChangeArtifact) => [
  { label: "Branch", value: artifact.start.worktree.branch },
  { label: "Starting HEAD", value: artifact.start.starting.head },
  { label: "Captured HEAD", value: artifact.candidate.head },
  { label: "Capture revision", value: artifact.revision },
];

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
  const loadEvidence = (id: string) => invoke<Evidence | null>("attempt_worktree_evidence", { projectId, runId: id });
  const evidence = useQuery({
    queryKey: ["git-attempt-diff", projectId, runId],
    queryFn: () => loadEvidence(runId), enabled: !!runId, retry: false,
  });
  const attemptOptions = attempts.data ?? [];
  const attempt = attemptOptions.find((item) => item.runId === runId);
  const artifact = !evidence.error && !attempt?.historyDeleted && evidence.data?.kind === "saved" ? evidence.data.artifact : undefined;
  function viewAttempt(id: string) {
    const conversationId = attemptOptions.find(({ runId }) => runId === id)?.conversationId;
    if (conversationId) openActivity(id, conversationId);
    else openActivity(id);
  }
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
        onClick={() => viewAttempt(runId)}>View attempt</Button>}
    </div>
    {artifact ? <RecordedArtifact key={`${runId}:${artifact.revision}`} artifact={artifact} projectId={projectId} runId={runId}
      attempts={attemptOptions} loadEvidence={loadEvidence} viewAttempt={viewAttempt} />
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

function RecordedArtifact({ artifact, projectId, runId, attempts, loadEvidence, viewAttempt }: {
  artifact: ChangeArtifact; projectId: string; runId: string; attempts: SavedAttempt[];
  loadEvidence: (id: string) => Promise<Evidence | null>; viewAttempt: (id: string) => void;
}) {
  const changes = artifact.candidate.changes;
  const allParts: Part[] = [
    ...(["committed", "staged", "working"] as const).map((id): Part => ({
      id, label: patchLabels[id], encoded: capturedPatch(artifact, id), language: "diff",
    })),
    ...(changes?.untracked ?? []).map((entry): Part => ({ id: `untracked:${entry.path}`, label: entry.path, encoded: entry.contentBase64, language: "plaintext", sha: entry.sha256 })),
  ];
  const parts = allParts.filter((entry) => entry.encoded || entry.sha);
  const [choice, select] = useState("");
  const [applying, setApplying] = useState<SavedPatchPart>();
  const [comparedPart, compare] = useState<SavedPatchPart>();
  const [applied, setApplied] = useState("");
  const { copy, run, feedback } = useContextActionFeedback();
  const part = parts.find((entry) => entry.id === choice) ?? parts[0];
  const text = decode(part?.encoded);
  const patchPart = part?.id === "committed" || part?.id === "staged" || part?.id === "working" ? part.id : undefined;
  const choices: AttemptChoice[] = attempts.filter((attempt) => !attempt.historyDeleted).map((attempt) => ({
    id: attempt.runId, runId: attempt.runId, label: `${attempt.label} · ${attempt.runId.slice(0, 8)}`,
    provenance: [{ label: "Checkout ID", value: attempt.checkoutId }],
  }));
  if (!choices.some(({ id }) => id === runId)) choices.unshift({ id: runId, runId, label: runId, provenance: [] });
  const commands: RecordedAction[] = [
    ...(text !== undefined ? [{ label: patchPart ? "Copy saved patch" : "Copy saved text", run: () => copy(text) }] : []),
    ...(part?.encoded ? [{ label: patchPart ? "Download saved patch" : "Download captured file",
      run: () => void run(async () => {
        const name = patchPart ? `${runId.slice(0, 12)}-${patchPart}.patch` : fileName(part.label);
        downloadArtifact(name, new Blob([decodeArtifactBytes(part.encoded!)], { type: patchPart ? "text/x-diff" : "application/octet-stream" }));
      }) }] : []),
    ...(patchPart && choices.length > 1 ? [{ label: "Compare with another saved attempt…", run: () => compare(patchPart) }] : []),
    ...(patchPart ? [{ label: "Apply saved patch…", run: () => setApplying(patchPart) }] : []),
  ];
  const identityActions: RecordedAction[] = [
    { label: "Copy capture revision", run: () => copy(artifact.revision) },
    { label: "Copy run ID", run: () => copy(runId) },
    { label: "Open recorded attempt", run: () => viewAttempt(runId) },
  ];
  return <div className="delivery-body">
    <div className="delivery-main flex min-w-0 flex-col">
      {part ? <>
        <div className="delivery-toolbar delivery-filters">
          <ContextActions label="Recorded diff source" groups={[commands, identityActions]} className="min-w-0">
            <label className="delivery-field min-w-0">Recorded diff source
            <select className="delivery-filter" value={part.id} onChange={(event) => select(event.target.value)}>
              {parts.map((entry) => <option key={entry.id} value={entry.id}>{entry.label}</option>)}
            </select>
            </label>
          </ContextActions>
          {part.sha && <p className="wrap-anywhere font-mono text-xs">SHA-256 {part.sha}</p>}
          {patchPart && <Button type="button" variant="outline" onClick={() => setApplying(patchPart)}>Apply saved patch…</Button>}
        </div>
        {feedback && <p className="px-3 py-2 text-sm" role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
        {applied && <p role="status" className="px-3 py-2 text-sm">Saved patch applied to {applied}.</p>}
        {text === undefined ? <p className="p-3 text-sm">Recorded text is unavailable for this file.</p>
          : <RecordedMonaco key={part.id} label={part.label} text={text} language={part.language}
            commands={[...commands, ...identityActions]} />}
      </> : <p className="p-3 text-sm">No patch was recorded for this attempt.</p>}
    </div>
    <DeliveryDetailPanel label="Recorded comparison identity" resizeKey="git-comparison-details">
      <div className="delivery-detail-content grid content-start gap-3 p-3.5">
        <ContextActions label="Capture details" groups={[identityActions]}><h3 className="text-sm font-semibold">Capture details</h3></ContextActions>
        <dl className="grid gap-2 text-xs">
          {captureDetails(artifact).map(({ label, value }) => <div key={label}>
            <dt className="text-muted-foreground">{label}</dt><dd className="wrap-anywhere font-mono">{value}</dd>
          </div>)}
        </dl>
      </div>
    </DeliveryDetailPanel>
    {applying && <SavedPatchDialog selection={{ projectId, runId, captureRevision: artifact.revision, part: applying }}
      close={() => setApplying(undefined)} applied={setApplied} />}
    {comparedPart && <SavedAttemptComparison title="Compare saved Git patches" description={patchLabels[comparedPart]}
      scope={["git-patch", projectId, runId, artifact.revision, comparedPart]} choices={choices} sourceId={runId}
      language="diff" close={() => compare(undefined)} openAttempt={viewAttempt} load={async (choice, signal) => {
        signal.throwIfAborted();
        const evidence = await loadEvidence(choice.runId);
        signal.throwIfAborted();
        if (evidence?.kind !== "saved") return;
        if (choice.runId === runId && evidence.artifact.revision !== artifact.revision) throw new Error("The displayed capture changed. Reopen the comparison.");
        const text = decode(capturedPatch(evidence.artifact, comparedPart));
        if (text !== undefined) return { text, provenance: captureDetails(evidence.artifact) };
      }} />}
  </div>;
}

function decode(encoded?: string) {
  if (encoded === undefined) return undefined;
  try {
    const value = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(decodeArtifactBytes(encoded));
    return value.includes("\0") ? undefined : value;
  } catch { return undefined; }
}

function RecordedMonaco({ label, text, language, commands }: { label: string; text: string; language: string; commands: RecordedAction[] }) {
  const host = useRef<HTMLDivElement>(null);
  const [error, setError] = useState(false);
  const activeCommands = useRef(commands);
  activeCommands.current = commands;
  const commandKey = commands.map(({ label }) => label).join("\0");
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
      activeCommands.current.forEach(({ label }, index) => editor!.addAction({
        id: `omniagent.saved-patch-${index}`, label, contextMenuGroupId: "9_omniagent-saved-patch", contextMenuOrder: index,
        run: () => activeCommands.current.find((command) => command.label === label)?.run(),
      }));
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
  }, [commandKey, label, language, text]);
  return error ? <ContextActions label={label} groups={[commands]} className="min-h-0 flex-1">
    <pre className="min-h-40 flex-1 overflow-auto whitespace-pre-wrap wrap-anywhere p-3 text-xs" aria-label={label}>{text}</pre>
  </ContextActions>
    : <div ref={host} className="min-h-45 flex-1" aria-label={label} />;
}
