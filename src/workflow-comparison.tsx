import { skipToken, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useId, useState } from "react";
import { Columns2 } from "lucide-react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ModelPicker, ModelRatingMeters } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { useContextActionFeedback } from "@/components/ui/context-actions";
import { PipelineComparisonCard, PipelineComparisonSetup } from "@/pipeline-comparison-layout";
import { PipelineSectionHeading } from "@/pipeline-layout";
import { sameModelSelection, useLiveModelOptions } from "@/live-models";
import type { WorkspaceProps } from "@/workbench-contract";
import { ComparisonCandidateOutcome, comparisonOutcome, type CandidateOutcome } from "@/workflow-comparison-outcomes";
import { selectableComparisonRun, type ComparisonGroup as Comparison } from "@/workflow-comparison-selection";
import { ComparisonSyntheses } from "@/workflow-comparison-synthesis";
import { readWorkflowComparisonNoteTarget } from "@/workflow-comparison-note-target";
import type { NoteRevision } from "@/repository-note-buffers";

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

export function WorkflowComparisons({ checkout, target, openActivity, openQueueJob }: WorkspaceProps) {
  const client = useQueryClient();
  const { projectId, checkoutId } = checkout;
  const fromNote = target?.startsWith("compare-note:") === true;
  const note = readWorkflowComparisonNoteTarget(target);
  const noteInCheckout = note?.checkoutId === checkoutId ? note : undefined;
  const [prompt, setPrompt] = useState("");
  const [commitOverride, setCommit] = useState<string>();
  const [models, setModels] = useState<ModelSelection[]>([]);
  const [activeId, setActiveId] = useState<string>();
  const [synthesisDraft, setSynthesisDraft] = useState<{ groupId: string; open: boolean; runIds: string[] }>();
  const evidencePrefix = useId();
  const { copy, feedback } = useContextActionFeedback();
  const modelsQuery = useLiveModelOptions();
  const source = useQuery({
    queryKey: ["comparison-source-commit", projectId, checkoutId],
    queryFn: () => invoke<string | null>("comparison_source_commit", { projectId, checkoutId }),
    enabled: !fromNote,
  });
  const noteSource = useQuery({
    queryKey: ["repository-markdown", checkoutId, noteInCheckout?.revisionId],
    queryFn: noteInCheckout ? () => invoke<{ revision: NoteRevision; markdown: string } | null>("read_repository_markdown", {
      checkoutId, revisionId: noteInCheckout.revisionId,
    }) : skipToken,
  });
  const recorded = noteSource.data?.revision;
  const savedNote = noteSource.isSuccess && noteInCheckout && recorded?.id === noteInCheckout.revisionId && recorded.checkoutId === checkoutId
    && recorded.relativePath === noteInCheckout.relativePath && recorded.sha256 === noteInCheckout.sha256 ? noteSource.data : undefined;
  const groupsKey = ["model-comparisons", projectId];
  const groups = useQuery({
    queryKey: groupsKey,
    queryFn: () => invoke<Comparison[]>("model_comparisons", { projectId }),
    refetchInterval: (query) => query.state.data?.some(active) ? 1200 : false,
  });
  const commit = fromNote ? savedNote?.revision.baseCommit ?? "" : commitOverride ?? source.data ?? "";
  const run = useMutation({
    mutationFn: () => {
      const input = { projectId, checkoutId, prompt: prompt.trim(), selections: models };
      if (!fromNote) return invoke<Comparison>("start_model_comparison", { ...input, commit: commit.trim() });
      if (!savedNote) throw new Error("The chosen Markdown revision is unavailable.");
      const { id: revisionId, relativePath, sha256 } = savedNote.revision;
      return invoke<Comparison>("start_model_comparison_from_markdown", { ...input, revisionId, relativePath, sha256 });
    },
    onSuccess: (group) => {
      client.setQueryData<Comparison[]>(groupsKey, (current) => [group, ...(current ?? [])]);
      setActiveId(group.id);
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

  return <section className="pipeline-stack min-h-0 overflow-auto p-4" aria-label="Model comparisons">
    <PipelineSectionHeading title="Compare models" icon={<Columns2 size={20} aria-hidden />} />
    <PipelineComparisonSetup onSubmit={(event) => { event.preventDefault(); if (canRun) run.mutate(); }} request={<>
      <label>Shared request<textarea rows={4} value={prompt} onChange={(event) => setPrompt(event.currentTarget.value)} /></label>
      {fromNote ? <>
        {!noteInCheckout && <p role="alert">This saved note target is unavailable in the current checkout.</p>}
        {noteInCheckout && noteSource.isPending && <p role="status">Loading the saved note revision…</p>}
        {noteSource.error && <p role="alert">{String(noteSource.error)}</p>}
        {noteSource.isSuccess && !savedNote && <p role="alert">The chosen Markdown revision is unavailable.</p>}
        {savedNote && <>
          <dl className="pipeline-data break-all" aria-label="Saved note input">
            <dt>Saved note</dt><dd>{savedNote.revision.relativePath}</dd>
            <dt>Note revision</dt><dd><code>{savedNote.revision.id}</code></dd>
            <dt>SHA-256</dt><dd><code>{savedNote.revision.sha256}</code></dd>
            {commit && <><dt>Git commit</dt><dd><code>{commit}</code></dd></>}
          </dl>
          <details className="pipeline-evidence"><summary>Read saved Markdown</summary>
            <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words">{savedNote.markdown}</pre>
          </details>
          {!commit && <p role="alert">This saved revision has no Git commit for the comparison checkout.</p>}
        </>}
      </> : <>
        <label>Git commit<input spellCheck={false} value={commit} onChange={(event) => setCommit(event.currentTarget.value)} placeholder="Full commit SHA" /></label>
        {source.error && <p role="alert">{String(source.error)}</p>}
        {source.data === null && <p className="text-sm text-muted-foreground">Create a Git commit to use this checkout for comparison.</p>}
      </>}
      <p className="text-sm text-muted-foreground">Each candidate runs independently in a clean, read-only checkout at this commit with the same request{fromNote ? " and saved note" : ""}.</p>
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
        setSynthesisDraft(undefined);
      }}>
        {groups.data?.map((group) => <option value={group.id} key={group.id}>{group.sourceDocument?.relativePath ?? group.prompt} · {group.commit.slice(0, 12)}</option>)}
      </select></label>
      <p className="break-all text-sm">Pinned Git commit <code>{result.commit}</code></p>
      {result.sourceDocument && <p className="break-all text-sm">Saved note <code>{result.sourceDocument.relativePath}</code> · Revision <code>{result.sourceDocument.id}</code> · SHA-256 <code>{result.sourceDocument.sha256}</code></p>}
      <h2 className="text-base font-medium">{comparisonOutcome(result.candidates.map(({ state }) => state))}</h2>
      {environment.error && <p role="alert">{String(environment.error)}</p>}
      {outcomes.error && <p role="alert">{String(outcomes.error)}</p>}
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      <ol className="pipeline-results" aria-label="Comparison results">
        {result.candidates.map((candidate) => {
          const runId = selectableComparisonRun(candidate, outcomes.data);
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
          </PipelineComparisonCard>;
        })}
      </ol>
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
