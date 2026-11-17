import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { editor } from "monaco-editor";
import { lazy, Suspense, useState } from "react";
import type { CommandJobOutput } from "@/bindings/CommandJobOutput";
import type { RunStatus } from "@/bindings/RunStatus";
import type { ChildLocation } from "@/conversation-children";
import { newTurnResponseEvidence, recordResponseEvidence, type ResponseObservation } from "@/conversation-response-evidence";
import { FileDialog } from "@/ide-file-workspace";
import type { WorkspaceProps } from "@/workbench-contract";

const MonacoComparison = lazy(() => import("@/git-diff-view").then(({ MonacoComparison }) => ({ default: MonacoComparison })));

export type AttemptChoice = { id: string; label: string; runId: string; provenance: readonly { label: string; value: string }[] };
type AttemptText = { text: string; provenance?: AttemptChoice["provenance"] };
type CompareLayout = "split" | "unified";

export function SavedAttemptComparison({ title, description, scope, choices, sourceId, language, load, close, openAttempt }: {
  title: string;
  description: string;
  scope: readonly string[];
  choices: readonly AttemptChoice[];
  sourceId: string;
  language: string;
  load: (choice: AttemptChoice, signal: AbortSignal) => Promise<AttemptText | undefined>;
  close: () => void;
  openAttempt: (runId: string) => void;
}) {
  const sourceIndex = choices.findIndex(({ id }) => id === sourceId);
  const initialLeft = choices.slice(0, sourceIndex).findLast(({ runId }) => runId !== choices[sourceIndex]?.runId)
    ?? choices.find(({ runId }) => runId !== choices[sourceIndex]?.runId);
  const [pair, setPair] = useState({ left: initialLeft?.id, right: sourceId });
  const [layout, setLayout] = useState<CompareLayout>("split");
  const [instance, setInstance] = useState<editor.IStandaloneDiffEditor | null>(null);
  const left = choices.find(({ id }) => id === pair.left);
  const right = choices.find(({ id }) => id === pair.right);
  const valid = Boolean(left && right && left.runId !== right.runId);
  const comparison = useQuery({
    queryKey: ["saved-attempt-comparison", ...scope, pair.left, pair.right],
    queryFn: async ({ signal }) => Promise.all([load(left!, signal), load(right!, signal)]),
    enabled: valid, retry: false, gcTime: 0, refetchOnWindowFocus: false,
  });
  const [original, modified] = comparison.data ?? [];
  const canCompare = Boolean(original && modified);
  return <FileDialog title={title} description={description} onClose={close} className="file-comparison-dialog">
    {sourceIndex < 0 || !initialLeft ? <p role="status">No other saved attempt is available in this scope.</p> : <>
      <div className="grid grid-cols-2 gap-3 text-sm">
        {(["left", "right"] as const).map((side) => {
          const attempt = side === "left" ? left : right;
          const other = side === "left" ? right : left;
          const snapshot = side === "left" ? original : modified;
          const label = side === "left" ? "Base attempt" : "Compared attempt";
          return <section key={side} aria-label={`${label} provenance`} className="min-w-0">
            <label className="grid gap-1" title={attempt?.label}>{label}
              <select className="delivery-filter min-w-0" value={pair[side]} onChange={(event) => setPair({ ...pair, [side]: event.currentTarget.value })}>
                {choices.map((choice) => <option key={choice.id} value={choice.id} disabled={choice.runId === other?.runId}>{choice.label}</option>)}
              </select>
            </label>
            {attempt && <>
              <dl className="grid gap-x-2 text-xs sm:grid-cols-[auto_1fr]">
                <dt>Run ID</dt><dd className="break-all"><code>{attempt.runId}</code></dd>
                {[...attempt.provenance, ...(snapshot?.provenance ?? [])].map(({ label, value }) =>
                  <div key={label} className="contents"><dt>{label}</dt><dd className="break-all">{value}</dd></div>)}
              </dl>
              <button type="button" className="fw-btn fw-outlined" onClick={() => { close(); openAttempt(attempt.runId); }}>Open {label.toLowerCase()}</button>
            </>}
          </section>;
        })}
      </div>
      {comparison.isPending && valid && <p role="status">Loading saved evidence…</p>}
      {comparison.error && <p role="alert">{String(comparison.error)}</p>}
      {comparison.data && !canCompare && <p role="status">{!original && !modified ? "Neither attempt has comparable saved output."
        : !original ? "The base attempt has no comparable saved output." : "The compared attempt has no comparable saved output."}</p>}
      {original && modified && <>
        <div className="flex flex-wrap gap-2">
          <label>Diff layout <select className="delivery-filter" value={layout} onChange={(event) => setLayout(event.currentTarget.value as CompareLayout)}>
            <option value="split">Side by side</option><option value="unified">Unified</option>
          </select></label>
          <button type="button" className="fw-btn" onClick={() => setPair({ left: pair.right, right: pair.left! })}>Swap attempts</button>
          <button type="button" className="fw-btn" disabled={!instance || original.text === modified.text} onClick={() => instance?.goToDiff("previous")}>Previous change</button>
          <button type="button" className="fw-btn" disabled={!instance || original.text === modified.text} onClick={() => instance?.goToDiff("next")}>Next change</button>
        </div>
        {original.text === modified.text && <p role="status">The saved text is identical.</p>}
        <div className="file-comparison-body" aria-label="Saved evidence diff">
          <Suspense fallback={<p role="status">Loading comparison editor…</p>}>
            <MonacoComparison layout={layout} ready={setInstance} file={{ path: title, language, original: original.text, modified: modified.text }}
              revisions={{ original: left!.id, modified: right!.id }} />
          </Suspense>
        </div>
      </>}
    </>}
  </FileDialog>;
}

type RelatedAttempt = { runId: string; checkoutId: string; status: RunStatus; ordinal: number; createdAt: number };
type ResponsePage = { cursor: string; more: boolean; events: { data: ResponseObservation; location: ChildLocation }[] };

async function conversationOutput(conversationId: string, runId: string, signal: AbortSignal): Promise<AttemptText | undefined> {
  const responses = newTurnResponseEvidence();
  let cursor = "0";
  for (;;) {
    signal.throwIfAborted();
    const page = await invoke<ResponsePage>("conversation_events", { conversationId, runId, after: cursor });
    if (page.more && page.cursor === cursor) throw new Error("The saved output cursor did not advance.");
    for (const event of page.events) if (["assistant_delta", "assistant_message", "assistant_final"].includes(event.data.type)) {
      recordResponseEvidence(responses, event.data, event.location);
    }
    cursor = page.cursor;
    if (!page.more) break;
  }
  const status = responses.items.some((item) => item.status === "final") ? "final"
    : responses.items.some((item) => item.status === "completed") ? "completed" : "streamed";
  const items = responses.items.filter((item) => item.status === status);
  if (!items.length) return;
  return { text: items.map(({ text }) => text).join("\n\n"), provenance: [
    { label: "Evidence", value: status === "final" ? "Provider-reported final response" : status === "completed" ? "Recorded response text" : "Streamed provisional text" },
    { label: "Recorded through event", value: cursor },
  ] };
}

async function commandOutput(projectId: string, runId: string): Promise<AttemptText | undefined> {
  const output = await invoke<CommandJobOutput | null>("command_job_output", { projectId, runId });
  if (!output) return;
  return { text: [`Standard output\n${output.stdout}`, `Standard error\n${output.stderr}`].join("\n\n"), provenance: [
    ...(output.process ? [{ label: "Exit code", value: String(output.process.code) }] : []),
    ...(output.truncated ? [{ label: "Capture", value: "Output capture is incomplete" }] : []),
  ] };
}

export function SavedRunOutputComparison({ projectId, conversationId, runId, close, openActivity }: {
  projectId: string; conversationId?: string; runId: string; close: () => void; openActivity: WorkspaceProps["openActivity"];
}) {
  const title = "Compare saved attempt output";
  const description = conversationId ? `Conversation ${conversationId}` : "Attempts of the same command job";
  const scope = [projectId, conversationId ?? "command", runId];
  const related = useQuery({ queryKey: ["related-saved-attempts", ...scope], retry: false, gcTime: 0,
    queryFn: () => invoke<RelatedAttempt[]>("related_saved_attempts", { projectId, runId, ...(conversationId ? { conversationId } : {}) }) });
  if (related.isPending || related.error) return <FileDialog title={title} description={description} onClose={close}>
    {related.error ? <p role="alert">{String(related.error)}</p> : <p role="status">Finding saved attempts…</p>}
  </FileDialog>;
  return <SavedAttemptComparison title={title} description={description} scope={scope} sourceId={runId} close={close}
    language={conversationId ? "markdown" : "plaintext"} openAttempt={(id) => conversationId ? openActivity(id, conversationId) : openActivity(id)}
    choices={(related.data ?? []).map((attempt) => ({ id: attempt.runId, runId: attempt.runId,
      label: `Attempt ${attempt.ordinal} · ${attempt.status.replaceAll("_", " ")}`,
      provenance: [{ label: "Checkout ID", value: attempt.checkoutId }, { label: "Run created", value: new Date(attempt.createdAt * 1000).toLocaleString() }],
    }))} load={(choice, signal) => conversationId ? conversationOutput(conversationId, choice.runId, signal) : commandOutput(projectId, choice.runId)} />;
}
