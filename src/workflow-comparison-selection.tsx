import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { ModelRating } from "@/bindings/ModelRating";
import { Button } from "@/components/ui/button";
import type { CandidateOutcome } from "@/workflow-comparison-outcomes";
import type { WorkspaceProps } from "@/workbench-contract";

export type ComparisonCandidate = {
  selection: ModelSelection;
  modelReference?: { name: string; rating: ModelRating | null; catalogVersion: number };
  jobId: string;
  runId: string | null;
  conversationId: string | null;
  dispatchId: string | null;
  state: string;
};
export type ComparisonGroup = { id: string; commit: string; tree: string; prompt: string; candidates: ComparisonCandidate[] };
export type ComparisonSelection = { groupId: string; runIds: string[] };
export type ComparisonInput = { groupId: string; jobId: string; runId: string; dispatchId: string; responseRevisionId: string };

export function ComparisonOutputChoices({ group, outcomes, selected, onChange }: {
  group: ComparisonGroup;
  outcomes?: CandidateOutcome[];
  selected: string[];
  onChange: (runIds: string[]) => void;
}) {
  const available = group.candidates.flatMap((candidate) => {
    const runId = candidate.runId;
    const hasResult = outcomes?.some((item) => item.runId === runId && item.jobId === candidate.jobId && item.response !== null);
    return runId && candidate.dispatchId && candidate.state === "succeeded" && hasResult ? [{ runId, model: candidate.selection.modelId }] : [];
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

export function ReviewComparisonInputs({ projectId, selection, onChange }: {
  projectId: string;
  selection: ComparisonSelection | undefined;
  onChange: (selection: ComparisonSelection | undefined) => void;
}) {
  const [open, setOpen] = useState(false);
  const [groupId, setGroup] = useState("");
  const groups = useQuery({ queryKey: ["model-comparisons", projectId], enabled: open,
    queryFn: () => invoke<ComparisonGroup[]>("model_comparisons", { projectId }) });
  const group = groups.data?.find(({ id }) => id === groupId);
  const outcomes = useQuery({ queryKey: ["comparison-outcomes", projectId, groupId,
    group?.candidates.map(({ runId, state }) => [runId, state])], enabled: Boolean(open && group),
    queryFn: () => invoke<CandidateOutcome[]>("model_comparison_outcomes", { projectId, groupId }) });
  return <details onToggle={(event) => setOpen(event.currentTarget.open)} className="pipeline-evidence">
    <summary>Use model comparison responses</summary>
    {open && <div className="pipeline-stack">
      <label className="pipeline-form">Source comparison
        <select value={groupId} onChange={(event) => { setGroup(event.currentTarget.value); onChange(undefined); }}>
          <option value="">Choose a comparison</option>
          {groups.data?.map((item) => <option value={item.id} key={item.id}>{item.prompt} · {item.commit.slice(0, 12)}</option>)}
        </select>
      </label>
      {(groups.error || outcomes.error) && <p role="alert">{String(groups.error || outcomes.error)}</p>}
      {group && <ComparisonOutputChoices group={group} outcomes={outcomes.data} selected={selection?.runIds ?? []}
        onChange={(runIds) => onChange(runIds.length ? { groupId, runIds } : undefined)} />}
    </div>}
  </details>;
}

export function SavedComparisonInputs({ projectId, jobId, openActivity }: {
  projectId: string; jobId: string; openActivity: WorkspaceProps["openActivity"];
}) {
  const [open, setOpen] = useState(false);
  const sources = useQuery({ queryKey: ["comparison-job-inputs", projectId, jobId], enabled: open,
    queryFn: () => invoke<ComparisonInput[]>("model_comparison_job_inputs", { projectId, jobId }) });
  return <details onToggle={(event) => setOpen(event.currentTarget.open)} className="pipeline-evidence">
    <summary>Comparison inputs for this attempt</summary>
    {sources.error && <p role="alert">{String(sources.error)}</p>}
    {sources.data && (sources.data.length ? <ComparisonInputsList inputs={sources.data} openActivity={openActivity} /> : <p>No comparison responses were used.</p>)}
  </details>;
}
