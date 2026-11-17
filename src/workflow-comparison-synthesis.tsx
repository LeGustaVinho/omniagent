import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { Button } from "@/components/ui/button";
import { ModelPicker } from "@/components/model-picker";
import { ConversationMarkdown } from "@/conversation-markdown";
import type { CandidateOutcome } from "@/workflow-comparison-outcomes";
import { ComparisonInputsList, ComparisonOutputChoices, selectableComparisonRun, type ComparisonGroup, type ComparisonInput } from "@/workflow-comparison-selection";
import type { WorkspaceProps } from "@/workbench-contract";

type Synthesis = {
  id: string;
  jobId: string;
  selection: ModelSelection;
  inputs: ComparisonInput[];
  runId: string | null;
  conversationId: string | null;
  dispatchId: string | null;
  state: string;
  response: string | null;
};

const terminal = new Set(["succeeded", "failed", "cancelled", "interrupted"]);

export function ComparisonSyntheses({ projectId, group, outcomes, options, openActivity, openQueueJob, open, runIds, onOpenChange, onChangeRuns }: {
  projectId: string;
  group: ComparisonGroup;
  outcomes?: CandidateOutcome[];
  options: ModelOption[];
  openActivity: WorkspaceProps["openActivity"];
  openQueueJob: WorkspaceProps["openQueueJob"];
  open: boolean;
  runIds: string[];
  onOpenChange: (open: boolean) => void;
  onChangeRuns: (ids: string[]) => void;
}) {
  const [selection, setSelection] = useState<ModelSelection | null>(null);
  const client = useQueryClient();
  const confirmedRuns = group.candidates.flatMap((candidate) => {
    const id = selectableComparisonRun(candidate, outcomes);
    return id && runIds.includes(id) ? [id] : [];
  });
  const key = ["comparison-syntheses", projectId, group.id];
  const syntheses = useQuery({ queryKey: key, enabled: open,
    queryFn: () => invoke<Synthesis[]>("model_comparison_syntheses", { projectId, groupId: group.id }),
    refetchInterval: (query) => query.state.data?.some(({ state }) => !terminal.has(state)) ? 1200 : false,
  });
  const create = useMutation({ mutationFn: () => invoke<Synthesis>("start_model_comparison_synthesis", {
    projectId, sources: { groupId: group.id, runIds: confirmedRuns }, selection,
  }), onSuccess: (row) => {
    client.setQueryData<Synthesis[]>(key, (current) => [row, ...(current ?? [])]);
    void client.invalidateQueries({ queryKey: key });
  } });
  return <details open={open} onToggle={(event) => onOpenChange(event.currentTarget.open)} className="pipeline-evidence">
    <summary>Synthesis</summary>
    {open && <div className="pipeline-stack">
      <form className="pipeline-form pipeline-stack" onSubmit={(event) => {
        event.preventDefault();
        if (selection && confirmedRuns.length && !create.isPending) create.mutate();
      }}>
        <ComparisonOutputChoices group={group} outcomes={outcomes} selected={confirmedRuns} onChange={onChangeRuns} />
        <ModelPicker label="Synthesis model" options={options} selection={{ mode: "single", selected: selection, onChange: setSelection }} />
        <Button type="submit" disabled={!selection || !confirmedRuns.length || create.isPending}>Queue synthesis</Button>
      </form>
      {(create.error || syntheses.error) && <p role="alert">{String(create.error || syntheses.error)}</p>}
      {syntheses.data?.map((result) => <article className="pipeline-box pipeline-stack" aria-label="Synthesis result" key={result.id}>
        <header><strong>{result.selection.modelId}</strong> <span role="status">{result.state.replaceAll("_", " ")}</span></header>
        <ComparisonInputsList inputs={result.inputs} openActivity={openActivity} />
        {result.response && <ConversationMarkdown>{result.response}</ConversationMarkdown>}
        {result.runId && result.conversationId ? <Button type="button" variant="outline"
          onClick={() => openActivity(result.runId!, result.conversationId!)}>Open synthesis execution</Button>
          : openQueueJob && <Button type="button" variant="outline" onClick={() => openQueueJob(result.jobId)}>Open synthesis job</Button>}
      </article>)}
    </div>}
  </details>;
}
