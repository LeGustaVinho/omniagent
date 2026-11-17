import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { Job } from "@/bindings/Job";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import { Button } from "@/components/ui/button";
import type { DeliveryActions, DeliveryInputs, DeliveryTarget } from "@/repository-delivery-model";
import type { WorkspaceProps } from "@/workbench-contract";

type StageInput = DeliveryTarget & {
  nodeId: string;
  unitId: string | null;
  inputs: DeliveryInputs;
  selection: ModelSelection;
  effort: string | null;
};

type GoalScope = {
  checkoutId: string;
  taskId: string;
  unitId: string;
  planRevision: string;
  releaseId: string;
  provider: Provider;
  profileId: string;
  model: string | null;
};

type GoalRequalification = {
  priorRevisionId: string;
  priorCheckpointRunId: string;
  priorScope: GoalScope;
  replacementScope: GoalScope;
  preservesWorktreeCheckpoint: boolean;
  turnsAlreadyUsed: number;
  reviewToken: string;
};

function GoalScopeDetails({ title, scope }: { title: string; scope: GoalScope }) {
  return <section aria-label={title} className="grid min-w-0 gap-1">
    <h4 className="font-medium">{title}</h4>
    <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1 text-xs">
      <dt>Plan</dt><dd className="break-all">{scope.planRevision}</dd>
      <dt>Release</dt><dd className="break-all">{scope.releaseId}</dd>
      <dt>Worktree</dt><dd className="break-all">{scope.checkoutId}</dd>
      <dt>Task</dt><dd className="break-all">{scope.taskId}</dd>
      <dt>Unit</dt><dd className="break-all">{scope.unitId}</dd>
      <dt>Model</dt><dd className="break-all">{scope.model ?? scope.provider} · {scope.profileId}</dd>
    </dl>
  </section>;
}

export function WorkflowGoalRequalification({ input, actions, openActivity }: {
  input: StageInput;
  actions: DeliveryActions;
  openActivity: WorkspaceProps["openActivity"];
}) {
  const inputKey = JSON.stringify(input);
  const [review, setReview] = useState<{ inputKey: string; result: GoalRequalification }>();
  const preview = useMutation({
    mutationFn: (input: StageInput) => invoke<GoalRequalification>("preview_workflow_goal_requalification", { input }),
    onSuccess: (result, reviewedInput) => setReview({ inputKey: JSON.stringify(reviewedInput), result }),
  });
  const current = review?.inputKey === inputKey ? review.result : undefined;
  const pending = preview.isPending || actions.busy;
  return <div className="pipeline-stack">
    {!current && <Button type="button" variant="outline" disabled={pending} onClick={() => preview.mutate(input)}>
      Review Goal continuation
    </Button>}
    {preview.error && JSON.stringify(preview.variables) === inputKey && <p role="alert">{String(preview.error)}</p>}
    {current && <section aria-label="Review Goal continuation" className="pipeline-form pipeline-stack">
      <div className="grid gap-3 sm:grid-cols-2">
        <GoalScopeDetails title="Previous scope" scope={current.priorScope} />
        <GoalScopeDetails title="New approved scope" scope={current.replacementScope} />
      </div>
      <p>{current.turnsAlreadyUsed} {current.turnsAlreadyUsed === 1 ? "turn" : "turns"} already used. The next attempt starts a fresh provider session.</p>
      {current.preservesWorktreeCheckpoint && <p>The worktree will be checked against the saved Git state before the next attempt.</p>}
      <div className="pipeline-actions">
        <Button type="button" variant="outline" onClick={() => openActivity(current.priorCheckpointRunId)}>
          Open previous checkpoint
        </Button>
        <Button type="button" disabled={pending} onClick={() => actions.execute(async () => {
          const job = await invoke<Job>("submit_workflow_goal_requalification", { input, reviewToken: current.reviewToken });
          setReview(undefined);
          return job;
        })}>Continue on approved scope</Button>
        <Button type="button" variant="outline" disabled={pending} onClick={() => setReview(undefined)}>Cancel review</Button>
      </div>
    </section>}
  </div>;
}
