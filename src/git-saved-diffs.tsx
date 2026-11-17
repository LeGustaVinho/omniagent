import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import type { IRange, editor as MonacoEditor } from "monaco-editor";
import { useEffect, useRef, useState } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { useCheckoutConversationActions } from "@/checkout-conversation-actions";
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
  start: { worktree: { branch: string; checkoutId: string }; starting: { head: string } };
  candidate: { head: string; changes?: { stagedPatchBase64: string; unstagedPatchBase64: string; untracked: Untracked[] } | null };
  committedPatchBase64: string;
};
type Evidence = { kind: "saved"; artifact: ChangeArtifact } | { kind: "unavailable"; reason: string };
type Part = { id: string; label: string; encoded?: string; language: "diff" | "plaintext"; sha?: string };
type RecordedAction = { label: string; run: () => void };
type PatchSelection = { text: string; firstLine: number; lastLine: number };
type ConversationDestination = { checkout: ProjectCheckout; open: (id: string) => Promise<void> };

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
  projectId: string; target?: string; worktree?: Pick<WorkspaceProps["checkout"], "checkoutId" | "checkoutRoot">;
  workspace?: Pick<WorkspaceProps, "checkout" | "openWorkspace" | "openCheckoutWorkspace">;
  openActivity: WorkspaceProps["openActivity"]; onClose?: () => void;
};

export function SavedGitDiffs(props: SavedGitDiffsProps) {
  return <RecordedAttempts key={JSON.stringify([props.projectId, props.target, props.worktree?.checkoutId])} {...props} />;
}

function RecordedAttempts({ projectId, target, worktree, workspace, openActivity, onClose }: SavedGitDiffsProps) {
  const requested = !worktree && target?.startsWith("attempt:") ? target.slice("attempt:".length) : undefined;
  const [selected, select] = useState(requested);
  const checkoutId = worktree?.checkoutId;
  const attempts = useQuery({ queryKey: ["git-saved-attempts", projectId, checkoutId],
    queryFn: () => invoke<SavedAttempt[]>("git_saved_attempts", checkoutId ? { projectId, checkoutId } : { projectId }), retry: false });
  const runId = selected ?? attempts.data?.[0]?.runId ?? "";
  const loadEvidence = (id: string) => invoke<Evidence | null>("attempt_worktree_evidence", { projectId, runId: id });
  const evidence = useQuery({
    queryKey: ["git-attempt-diff", projectId, runId],
    queryFn: () => loadEvidence(runId), enabled: !!runId, retry: false,
  });
  const attemptOptions = attempts.data ?? [];
  const attempt = attemptOptions.find((item) => item.runId === runId);
  const artifact = !evidence.error && !attempt?.historyDeleted && evidence.data?.kind === "saved" ? evidence.data.artifact : undefined;
  const sourceCheckoutId = artifact?.start.worktree.checkoutId ?? attempt?.checkoutId;
  const lookupSource = Boolean(workspace?.openCheckoutWorkspace && sourceCheckoutId && sourceCheckoutId !== workspace.checkout.checkoutId);
  const registered = useQuery({ queryKey: ["registered-checkouts"], queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts"), enabled: lookupSource });
  const sourceCheckout = registered.data?.find((entry) => entry.projectId === projectId && entry.checkoutId === sourceCheckoutId);
  const conversation: ConversationDestination | undefined = workspace?.checkout.projectId === projectId && (!lookupSource || !registered.isPending) ? {
    checkout: sourceCheckout && workspace.openCheckoutWorkspace ? sourceCheckout : workspace.checkout,
    open: async (id) => {
      if (sourceCheckout && workspace.openCheckoutWorkspace && sourceCheckout.checkoutId !== workspace.checkout.checkoutId) {
        await workspace.openCheckoutWorkspace(sourceCheckout, { workspace: "conversations", target: id });
      } else workspace.openWorkspace("conversations", id);
    },
  } : undefined;
  function viewAttempt(id: string) {
    const conversationId = attemptOptions.find(({ runId }) => runId === id)?.conversationId;
    if (conversationId) openActivity(id, conversationId);
    else openActivity(id);
  }
  return <GitDiffFrame source="saved" title="Saved diff" onClose={onClose}>
    {worktree && <p className="border-b border-border px-3 py-2 text-sm">Worktree <code className="wrap-anywhere">{worktree.checkoutRoot}</code></p>}
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
    {artifact ? <RecordedArtifact key={JSON.stringify([runId, artifact.revision, conversation?.checkout.checkoutId])} artifact={artifact} projectId={projectId} runId={runId}
      attempts={attemptOptions} sourceCheckoutId={sourceCheckoutId} conversation={conversation}
      loadEvidence={loadEvidence} viewAttempt={viewAttempt} />
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

function RecordedArtifact({ artifact, projectId, runId, attempts, sourceCheckoutId, conversation, loadEvidence, viewAttempt }: {
  artifact: ChangeArtifact; projectId: string; runId: string; attempts: SavedAttempt[];
  sourceCheckoutId?: string; conversation?: ConversationDestination;
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
  const draft = useCheckoutConversationActions(conversation?.checkout.checkoutId ?? "", !conversation, (id) => {
    if (conversation) void run(async () => {
      try { await conversation.open(id); }
      catch (error) { throw new Error(`The draft was updated. Could not open the conversation: ${String(error)}`); }
    });
  });
  const part = parts.find((entry) => entry.id === choice) ?? parts[0];
  const text = decode(part?.encoded);
  const patchPart = part?.id === "committed" || part?.id === "staged" || part?.id === "working" ? part.id : undefined;
  function reviewSelection(selection: PatchSelection) {
    if (!patchPart || !conversation || !sourceCheckoutId) return;
    const { firstLine, lastLine } = selection;
    const lines = firstLine === lastLine ? `patch line ${firstLine}` : `patch lines ${firstLine}–${lastLine}`;
    draft.startContextText({ title: "Saved Git patch review", text: selection.text, language: "diff",
      reference: `${patchLabels[patchPart]} · ${lines} · Run ${runId} · Capture ${artifact.revision} · Worktree ${sourceCheckoutId} · Starting HEAD ${artifact.start.starting.head} · Captured HEAD ${artifact.candidate.head}`,
      confirmSource: async () => {
        const saved = await loadEvidence(runId);
        if (saved?.kind !== "saved" || saved.artifact.revision !== artifact.revision || capturedPatch(saved.artifact, patchPart) !== part?.encoded) {
          throw new Error("This saved patch changed or was removed. Reopen the recorded attempt to review its current evidence.");
        }
      },
    }, "Review this excerpt from the saved Git patch. Explain any issues you find in these changes.");
  }
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
            commands={[...commands, ...identityActions]}
            reviewSelection={patchPart && conversation && sourceCheckoutId ? reviewSelection : undefined} />}
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
    {draft.dialog}
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

const reviewExcerptAction = "Review selected patch excerpt…";

function patchLineOffsets(text: string) {
  return [0, ...Array.from(text.matchAll(/\r\n|\r|\n/g), (breakAt) => breakAt.index + breakAt[0].length)];
}

function RecordedMonaco({ label, text, language, commands, reviewSelection }: {
  label: string; text: string; language: string; commands: RecordedAction[]; reviewSelection?: (selection: PatchSelection) => void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const [error, setError] = useState(false);
  const activeCommands = useRef(commands);
  const onReview = useRef(reviewSelection);
  activeCommands.current = commands;
  onReview.current = reviewSelection;
  const canReview = Boolean(reviewSelection);
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
      if (canReview) {
        const sourceLines = patchLineOffsets(text);
        let clickedSelection: IRange | null | undefined;
        editor.onMouseDown(() => { clickedSelection = undefined; });
        editor.onKeyDown(() => { clickedSelection = undefined; });
        editor.onContextMenu(({ target }) => {
          const selection = editor?.getSelection();
          clickedSelection = target.position && selection?.containsPosition(target.position) ? selection : null;
        });
        editor.addAction({ id: "omniagent.review-saved-patch-selection", label: reviewExcerptAction, precondition: "editorHasSelection",
          contextMenuGroupId: "9_omniagent-saved-patch", contextMenuOrder: commands.length,
          run: () => {
            const selection = clickedSelection === undefined ? editor?.getSelection() : clickedSelection;
            clickedSelection = undefined;
            if (!selection) return;
            const { startLineNumber, startColumn, endLineNumber, endColumn } = selection;
            const excerpt = text.slice(sourceLines[startLineNumber - 1]! + startColumn - 1, sourceLines[endLineNumber - 1]! + endColumn - 1);
            if (excerpt.trim()) onReview.current?.({ text: excerpt, firstLine: startLineNumber,
              lastLine: endColumn === 1 && endLineNumber > startLineNumber ? endLineNumber - 1 : endLineNumber });
          },
        });
      }
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
  }, [canReview, commandKey, commands.length, label, language, text]);
  return error ? <RecordedPatchFallback label={label} text={text} commands={commands} reviewSelection={reviewSelection} />
    : <div ref={host} className="min-h-45 flex-1" aria-label={label} />;
}

function RecordedPatchFallback({ label, text, commands, reviewSelection }: {
  label: string; text: string; commands: RecordedAction[]; reviewSelection?: (selection: PatchSelection) => void;
}) {
  const pre = useRef<HTMLPreElement>(null);
  const [selected, select] = useState<PatchSelection>();
  function capture() {
    if (!reviewSelection || !pre.current) return;
    const host = pre.current;
    const selection = host.ownerDocument.getSelection();
    const range = selection?.rangeCount ? selection.getRangeAt(0) : undefined;
    if (!range || range.collapsed || !host.contains(range.startContainer) || !host.contains(range.endContainer)) {
      select(undefined);
      return;
    }
    const prefix = host.ownerDocument.createRange();
    prefix.selectNodeContents(host);
    prefix.setEnd(range.startContainer, range.startOffset);
    const start = prefix.toString().length;
    const excerpt = range.toString();
    const lastCharacter = start + excerpt.length - 1;
    const lines = patchLineOffsets(text);
    select(excerpt.trim() ? { text: excerpt, firstLine: lines.filter((offset) => offset <= start).length,
      lastLine: lines.filter((offset) => offset <= lastCharacter).length } : undefined);
  }
  return <div className="flex min-h-0 flex-1" onContextMenuCapture={capture}
    onKeyDownCapture={(event) => { if (event.key === "ContextMenu" || event.shiftKey && event.key === "F10") capture(); }}
    onPointerDownCapture={(event) => { if (event.target instanceof Element && event.target.closest(".context-actions-more")) capture(); }}>
    <ContextActions label={label} groups={[[...commands, ...(reviewSelection && selected
      ? [{ label: reviewExcerptAction, run: () => reviewSelection(selected) }] : [])]]} className="min-h-0 flex-1">
      <pre ref={pre} tabIndex={0} className="min-h-40 flex-1 overflow-auto whitespace-pre-wrap wrap-anywhere p-3 text-xs" aria-label={label}>{text}</pre>
    </ContextActions>
  </div>;
}
