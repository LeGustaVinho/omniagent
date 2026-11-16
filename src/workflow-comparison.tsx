import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ModelPicker, ModelRatingMeters } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { sameModelSelection, useLiveModelOptions } from "@/live-models";
import type { WorkspaceProps } from "@/workbench-contract";

type Candidate = {
  selection: ModelSelection;
  jobId: string;
  runId: string | null;
  conversationId: string | null;
  dispatchId: string | null;
  state: string;
};

type Comparison = { id: string; commit: string; tree: string; prompt: string; candidates: Candidate[] };
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

export function WorkflowComparisons({ checkout, openActivity, openQueueJob }: WorkspaceProps) {
  const client = useQueryClient();
  const { projectId, checkoutId } = checkout;
  const [prompt, setPrompt] = useState("");
  const [commitOverride, setCommit] = useState<string>();
  const [models, setModels] = useState<ModelSelection[]>([]);
  const [activeId, setActiveId] = useState<string>();
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
    },
  });
  const canRun = Boolean(prompt.trim()) && validCommit.test(commit.trim()) && models.length > 1 && !run.isPending;
  const result = groups.data?.find(({ id }) => id === activeId) ?? groups.data?.[0];
  const groupId = result?.id;
  const environment = useQuery({
    queryKey: ["comparison-environment", projectId, groupId, result?.candidates.map(({ dispatchId, state }) => [dispatchId, state])],
    queryFn: () => invoke<Environment[]>("model_comparison_environments", { projectId, groupId }),
    enabled: Boolean(groupId && result?.candidates.some(({ dispatchId }) => dispatchId)),
    refetchInterval: () => result && active(result) ? 1200 : false,
  });

  return <section className="pipeline-stack min-h-0 overflow-auto p-4" aria-label="Model comparisons">
    <h1 className="text-lg font-semibold">Compare models</h1>
    <form className="pipeline-stack pipeline-form pipeline-box" onSubmit={(event) => { event.preventDefault(); if (canRun) run.mutate(); }}>
      <label>Shared request<textarea rows={4} value={prompt} onChange={(event) => setPrompt(event.currentTarget.value)} /></label>
      <label>Git commit<input spellCheck={false} value={commit} onChange={(event) => setCommit(event.currentTarget.value)} placeholder="Full commit SHA" /></label>
      {source.error && <p role="alert">{String(source.error)}</p>}
      {source.data === null && <p className="text-sm text-muted-foreground">Create a Git commit to use this checkout for comparison.</p>}
      <ModelPicker label="Comparison candidates" options={modelsQuery.options} loading={modelsQuery.loading}
        selection={{ mode: "multiple", selected: models, onChange: setModels }} />
      {models.length > 0 && <ol className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3" aria-label="Candidate order">
        {models.map((selection) => {
          const option = modelsQuery.options.find((entry) => sameModelSelection(entry.selection, selection));
          return <li key={JSON.stringify(selection)} className="rounded-md border border-border p-2 text-sm">
            <strong>{option?.name ?? selection.modelId}</strong> <small>{selection.provider}</small>
            <ModelRatingMeters rating={option?.rating ?? null} />
          </li>;
        })}
      </ol>}
      <p className="text-sm text-muted-foreground">Each candidate runs independently using the same request and a clean, read-only checkout at this commit.</p>
      {run.error && <p role="alert">{String(run.error)}</p>}
      <Button type="submit" disabled={!canRun}>{run.isPending ? "Preparing comparison…" : "Start comparison"}</Button>
    </form>
    {groups.error && <p role="alert">{String(groups.error)}</p>}
    {result && <section className="pipeline-stack pipeline-box" aria-label="Saved comparison">
      <label className="pipeline-form">Comparison <select value={result.id} onChange={(event) => setActiveId(event.currentTarget.value)}>
        {groups.data?.map((group) => <option value={group.id} key={group.id}>{group.prompt} · {group.commit.slice(0, 12)}</option>)}
      </select></label>
      <p className="break-all text-sm">Pinned Git commit <code>{result.commit}</code></p>
      {environment.error && <p role="alert">{String(environment.error)}</p>}
      <ol className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3" aria-label="Comparison results">
        {result.candidates.map((candidate) => <li key={candidate.jobId} className="pipeline-box pipeline-stack">
          <header><h2>{candidate.selection.modelId}</h2><small>{candidate.selection.provider}</small></header>
          <p role="status">{candidate.state.replaceAll("_", " ")}</p>
          <ComparisonEnvironment environment={environment.data?.find((entry) => entry.jobId === candidate.jobId && entry.dispatchId === candidate.dispatchId)} />
          {candidate.runId && candidate.conversationId && <Button type="button" variant="outline"
            onClick={() => openActivity(candidate.runId!, candidate.conversationId!)}>Open execution</Button>}
          {!candidate.runId && openQueueJob && <Button type="button" variant="outline" onClick={() => openQueueJob(candidate.jobId)}>Open queued job</Button>}
        </li>)}
      </ol>
    </section>}
  </section>;
}

function ComparisonEnvironment({ environment }: { environment?: Environment }) {
  if (!environment) return null;
  return <details className="pipeline-evidence text-sm">
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
