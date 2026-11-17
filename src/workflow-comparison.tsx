import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useId, useState } from "react";
import { Columns2 } from "lucide-react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ModelPicker, ModelRatingMeters } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { useContextActionFeedback } from "@/components/ui/context-actions";
import { PipelineComparisonCard, PipelineComparisonChoice, PipelineComparisonSetup } from "@/pipeline-comparison-layout";
import { PipelineSectionHeading } from "@/pipeline-layout";
import { sameModelSelection, useLiveModelOptions } from "@/live-models";
import type { WorkspaceProps } from "@/workbench-contract";
import { ComparisonCandidateOutcome, comparisonOutcome, type CandidateOutcome } from "@/workflow-comparison-outcomes";
import { selectableComparisonRun, type ComparisonGroup as Comparison, type ComparisonSelection } from "@/workflow-comparison-selection";
import { WorkflowComparisonDestination, type ComparisonHandoff } from "@/workflow-comparison-handoff";
import { ComparisonSyntheses } from "@/workflow-comparison-synthesis";

type Environment = {
  jobId: string;
  dispatchId: string;
  instructions: { path: string; scope: string; revision: string | null; selection: string; observedRead: boolean }[];
  reportedTools: string[];
  observedTools: string[];
};

const terminal = new Set(["succeeded", "failed", "cancelled", "interrupted"]);
const active = (comparison: Comparison) => comparison.candidates.some(({ state }) => !terminal.has(state));
const validCommit = /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/i;

export function WorkflowComparisons({ checkout, openActivity, openQueueJob, onUseInPipeline }: WorkspaceProps & {
  onUseInPipeline?: (handoff: ComparisonHandoff) => void;
}) {
  const client = useQueryClient();
  const { projectId, checkoutId } = checkout;
  const [prompt, setPrompt] = useState("");
  const [commitOverride, setCommit] = useState<string>();
  const [models, setModels] = useState<ModelSelection[]>([]);
  const [activeId, setActiveId] = useState<string>();
  const [included, setIncluded] = useState<ComparisonSelection>();
  const [destination, setDestination] = useState<string>();
  const [synthesisDraft, setSynthesisDraft] = useState<{ groupId: string; open: boolean; runIds: string[] }>();
  const evidencePrefix = useId();
  const { copy, feedback } = useContextActionFeedback();
  const modelsQuery = useLiveModelOptions();
  const source = useQuery({
    queryKey: ["comparison-source-commit", projectId, checkoutId],
    queryFn: () => invoke<string | null>("comparison_source_commit", { projectId, checkoutId }),
  });
  const groupsKey = ["model-comparisons", projectId];
  const groups = useQuery({
    queryKey: groupsKey,
    queryFn: () => invoke<Comparison[]>("model_comparisons", { projectId }),
    refetchInterval: (query) => query.state.data?.some(active) ? 1200 : false,
  });
  const commit = commitOverride ?? source.data ?? "";
  const run = useMutation({
    mutationFn: () => invoke<Comparison>("start_model_comparison", { projectId, checkoutId, commit: commit.trim(), prompt: prompt.trim(), selections: models }),
    onSuccess: (group) => {
      client.setQueryData<Comparison[]>(groupsKey, (current) => [group, ...(current ?? [])]);
      setActiveId(group.id);
      setIncluded(undefined);
      setDestination(undefined);
      setSynthesisDraft(undefined);
    },
  });
  const canRun = Boolean(prompt.trim()) && validCommit.test(commit.trim()) && models.length > 1 && !run.isPending;
  const result = groups.data?.find(({ id }) => id === activeId) ?? groups.data?.[0];
  const groupId = result?.id;
  const synthesisOpen = synthesisDraft?.groupId === groupId && Boolean(synthesisDraft?.open);
  const synthesisRuns = synthesisDraft?.groupId === groupId ? synthesisDraft?.runIds ?? [] : [];
  const environment = useQuery({
    queryKey: ["comparison-environment", projectId, groupId, result?.candidates.map(({ dispatchId, state }) => [dispatchId, state])],
    queryFn: () => invoke<Environment[]>("model_comparison_environments", { projectId, groupId }),
    enabled: Boolean(groupId && result?.candidates.some(({ dispatchId }) => dispatchId)),
    refetchInterval: () => result && active(result) ? 1200 : false,
  });
  const outcomes = useQuery({
    queryKey: ["comparison-outcomes", projectId, groupId, result?.candidates.map(({ runId, state }) => [runId, state])],
    queryFn: () => invoke<CandidateOutcome[]>("model_comparison_outcomes", { projectId, groupId }),
    enabled: Boolean(groupId && result?.candidates.some(({ state, runId }) => runId && terminal.has(state))),
  });
  const selectedRuns = included && included.groupId === groupId ? included.runIds : [];
  const sourceRuns = result?.candidates.flatMap((candidate) => {
    const runId = selectableComparisonRun(candidate, outcomes.data);
    return runId && selectedRuns.includes(runId) ? [runId] : [];
  }) ?? [];

  return <section className="pipeline-stack min-h-0 overflow-auto p-4" aria-label="Model comparisons">
    <PipelineSectionHeading title="Compare models" icon={<Columns2 size={20} aria-hidden />} />
    <PipelineComparisonSetup onSubmit={(event) => { event.preventDefault(); if (canRun) run.mutate(); }} request={<>
      <label>Shared request<textarea rows={4} value={prompt} onChange={(event) => setPrompt(event.currentTarget.value)} /></label>
      <label>Git commit<input spellCheck={false} value={commit} onChange={(event) => setCommit(event.currentTarget.value)} placeholder="Full commit SHA" /></label>
      {source.error && <p role="alert">{String(source.error)}</p>}
      {source.data === null && <p className="text-sm text-muted-foreground">Create a Git commit to use this checkout for comparison.</p>}
      <p className="text-sm text-muted-foreground">Each candidate runs independently using the same request and a clean, read-only checkout at this commit.</p>
      {run.error && <p role="alert">{String(run.error)}</p>}
      <Button type="submit" disabled={!canRun}>{run.isPending ? "Preparing comparison…" : "Start comparison"}</Button>
    </>} candidates={<>
      <ModelPicker label="Comparison candidates" options={modelsQuery.options} loading={modelsQuery.loading}
        selection={{ mode: "multiple", selected: models, onChange: setModels }} />
      {models.length > 0 && <ol className="grid gap-2" aria-label="Candidate order">
        {models.map((selection) => {
          const option = modelsQuery.options.find((entry) => sameModelSelection(entry.selection, selection));
          return <li key={JSON.stringify(selection)} className="rounded-md border border-border p-2 text-sm">
            <strong>{option?.name ?? selection.modelId}</strong> <small>{selection.provider}</small>
            <ModelRatingMeters rating={option?.rating ?? null} />
          </li>;
        })}
      </ol>}
    </>} />
    {groups.error && <p role="alert">{String(groups.error)}</p>}
    {result && <section className="pipeline-stack pipeline-box" aria-label="Saved comparison">
      <label className="pipeline-form">Comparison <select value={result.id} onChange={(event) => {
        setActiveId(event.currentTarget.value);
        setIncluded(undefined);
        setDestination(undefined);
        setSynthesisDraft(undefined);
      }}>
        {groups.data?.map((group) => <option value={group.id} key={group.id}>{group.prompt} · {group.commit.slice(0, 12)}</option>)}
      </select></label>
      <p className="break-all text-sm">Pinned Git commit <code>{result.commit}</code></p>
      <h2 className="text-base font-medium">{comparisonOutcome(result.candidates.map(({ state }) => state))}</h2>
      {environment.error && <p role="alert">{String(environment.error)}</p>}
      {outcomes.error && <p role="alert">{String(outcomes.error)}</p>}
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      <ol className="pipeline-results" aria-label="Comparison results">
        {result.candidates.map((candidate) => {
          const runId = selectableComparisonRun(candidate, outcomes.data);
          const sourceRun = onUseInPipeline && result.sourceCheckoutId && runId;
          const answer = outcomes.data?.find((row) => row.jobId === candidate.jobId && row.runId === candidate.runId);
          const runtime = environment.data?.find((entry) => entry.jobId === candidate.jobId && entry.dispatchId === candidate.dispatchId);
          const evidenceId = `${evidencePrefix}-${candidate.jobId}`;
          const selectedInSynthesis = Boolean(runId && synthesisRuns.includes(runId));
          const rating = candidate.modelReference
            ? candidate.modelReference.rating
            : modelsQuery.options.find((entry) => sameModelSelection(entry.selection, candidate.selection))?.rating ?? null;
          return <PipelineComparisonCard key={candidate.jobId} name={candidate.modelReference?.name ?? candidate.selection.modelId}
            state={candidate.state} rating={rating} actions={[[
              ...(typeof answer?.response === "string" ? [{ label: "Copy original response", run: () => copy(answer.response!) }] : []),
              ...(runtime ? [{ label: "Open instructions and tools", run: () => {
                const details = document.getElementById(evidenceId) as HTMLDetailsElement | null;
                if (details) { details.open = true; details.querySelector("summary")?.focus(); }
              } }] : []),
              ...(candidate.runId ? [{ label: "Open execution evidence", run: () => openActivity(candidate.runId!, candidate.conversationId ?? undefined) }]
                : openQueueJob ? [{ label: "Open queued job", run: () => openQueueJob(candidate.jobId) }] : []),
            ], [
              ...(runId ? [{ label: selectedInSynthesis ? "Remove from synthesis" : "Include in synthesis", run: () => {
                setSynthesisDraft({ groupId: result.id, open: true,
                  runIds: selectedInSynthesis ? synthesisRuns.filter((id) => id !== runId) : [...synthesisRuns, runId] });
              } }] : []),
              { label: "Copy job ID", run: () => copy(candidate.jobId) },
              ...(candidate.runId ? [{ label: "Copy run ID", run: () => copy(candidate.runId!) }] : []),
              ...(candidate.dispatchId ? [{ label: "Copy dispatch ID", run: () => copy(candidate.dispatchId!) }] : []),
            ]]}>
            <p className="pipeline-meta">{candidate.selection.provider}</p>
            <ComparisonCandidateOutcome outcome={answer} succeeded={candidate.state === "succeeded"} />
            <ComparisonEnvironment environment={runtime} id={evidenceId} />
            {candidate.runId && candidate.conversationId && <Button type="button" variant="outline"
              onClick={() => openActivity(candidate.runId!, candidate.conversationId!)}>Open execution</Button>}
            {!candidate.runId && openQueueJob && <Button type="button" variant="outline" onClick={() => openQueueJob(candidate.jobId)}>Open queued job</Button>}
            {sourceRun && <PipelineComparisonChoice checked={selectedRuns.includes(sourceRun)} onChange={(checked) =>
              setIncluded({ groupId: result.id, runIds: checked ? [...selectedRuns, sourceRun] : selectedRuns.filter((id) => id !== sourceRun) })} />}
          </PipelineComparisonCard>;
        })}
      </ol>
      {onUseInPipeline && result.sourceCheckoutId && !active(result) &&
        result.candidates.some((candidate) => selectableComparisonRun(candidate, outcomes.data)) && <div className="pipeline-actions">
          <Button type="button" variant="outline" disabled={!sourceRuns.length} aria-expanded={destination === result.id}
            onClick={() => setDestination(destination === result.id ? undefined : result.id)}>Use in pipeline</Button>
        </div>}
      {onUseInPipeline && destination === result.id && <WorkflowComparisonDestination key={`destination:${result.id}`} projectId={projectId}
        sourceCheckoutId={result.sourceCheckoutId} selection={{ groupId: result.id, runIds: sourceRuns }} onUse={onUseInPipeline} />}
      <ComparisonSyntheses key={result.id} projectId={projectId} group={result} outcomes={outcomes.data} options={modelsQuery.options}
        openActivity={openActivity} openQueueJob={openQueueJob} open={synthesisOpen} runIds={synthesisRuns}
        onOpenChange={(open) => { if (open !== synthesisOpen) setSynthesisDraft({ groupId: result.id, open, runIds: synthesisRuns }); }}
        onChangeRuns={(runIds) => setSynthesisDraft({ groupId: result.id, open: synthesisOpen, runIds })} />
    </section>}
  </section>;
}

function ComparisonEnvironment({ environment, id }: { environment?: Environment; id: string }) {
  if (!environment) return null;
  return <details className="pipeline-evidence text-sm" id={id}>
    <summary>Instructions and tools</summary>
    <p className="break-all">Dispatch <code>{environment.dispatchId}</code></p>
    {environment.instructions.length > 0 && <ul aria-label="Instruction sources" className="grid gap-1">
      {environment.instructions.map((source) => <li className="break-all" key={`${source.scope}:${source.path}`}>
        <code>{source.path}</code> <span>{source.observedRead ? "Observed read" : source.selection.replaceAll("_", " ")}</span>
        {source.revision && <small> · {source.revision.slice(0, 12)}</small>}
      </li>)}
    </ul>}
    {environment.reportedTools.length > 0 && <p>Reported tools · {environment.reportedTools.join(", ")}</p>}
    {environment.observedTools.length > 0 && <p>Tools used · {environment.observedTools.join(", ")}</p>}
  </details>;
}
