import type { ModelSelection } from "@/bindings/ModelSelection";
import type { ModelRating } from "@/bindings/ModelRating";
import { Button } from "@/components/ui/button";
import type { CandidateOutcome } from "@/workflow-comparison-outcomes";
import type { WorkspaceProps } from "@/workbench-contract";
import type { NoteRevision } from "@/repository-note-buffers";

export type ComparisonCandidate = {
  selection: ModelSelection;
  modelReference?: { name: string; rating: ModelRating | null; catalogVersion: number };
  jobId: string;
  runId: string | null;
  conversationId: string | null;
  dispatchId: string | null;
  state: string;
};
export type ComparisonGroup = { id: string; sourceCheckoutId: string; commit: string; tree: string; prompt: string;
  sourceDocument?: NoteRevision | null; candidates: ComparisonCandidate[] };
export type ComparisonInput = { groupId: string; jobId: string; runId: string; dispatchId: string; responseRevisionId: string };

export function selectableComparisonRun(candidate: ComparisonCandidate, outcomes?: CandidateOutcome[]) {
  const { runId, jobId, dispatchId, state } = candidate;
  const outcome = outcomes?.find((item) => item.runId === runId && item.jobId === jobId);
  return runId && dispatchId && state === "succeeded" && typeof outcome?.response === "string" ? runId : null;
}

export function ComparisonOutputChoices({ group, outcomes, selected, onChange }: {
  group: ComparisonGroup;
  outcomes?: CandidateOutcome[];
  selected: string[];
  onChange: (runIds: string[]) => void;
}) {
  const available = group.candidates.flatMap((candidate) => {
    const runId = selectableComparisonRun(candidate, outcomes);
    return runId ? [{ runId, model: candidate.selection.modelId }] : [];
  });
  return <fieldset className="grid gap-1">
    <legend>Successful responses</legend>
    {available.length === 0 && <p className="text-sm text-muted-foreground">No saved responses are available to select.</p>}
    {available.map(({ runId, model }) => <label className="flex items-center gap-2" key={runId}>
      <input type="checkbox" checked={selected.includes(runId)} onChange={(event) => {
        const included = event.currentTarget.checked ? [...selected, runId] : selected.filter((id) => id !== runId);
        onChange(available.map((item) => item.runId).filter((id) => included.includes(id)));
      }} />
      <span>{model} · <code>{runId}</code></span>
    </label>)}
  </fieldset>;
}

export function ComparisonInputsList({ inputs, openActivity }: { inputs: ComparisonInput[]; openActivity: WorkspaceProps["openActivity"] }) {
  return <ol className="grid gap-1 text-sm" aria-label="Source comparison runs">
    {inputs.map(({ runId, dispatchId }) => <li className="break-all" key={runId}>
      Run <code>{runId}</code> · Dispatch <code>{dispatchId}</code>
      <Button type="button" variant="outline" onClick={() => openActivity(runId)}>Open source execution</Button>
    </li>)}
  </ol>;
}
