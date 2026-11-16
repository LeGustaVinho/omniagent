import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Job } from "@/bindings/Job";
import { Button } from "@/components/ui/button";
import { ModelPicker, ModelRatingMeters } from "@/components/model-picker";
import { availableModelSelection, sameModelSelection, useLiveModelOptions } from "@/live-models";
import { useProjectModelDefaults } from "@/model-defaults";
import {
  deliveryProviders, type DeliveryActions, type DeliveryInputs,
  type DeliveryModelChoice, type DeliveryNode, type DeliveryTarget,
} from "@/repository-delivery-model";
import { useTaskAssignments } from "@/task-assignment";
import type { WorkspaceProps } from "@/workbench-contract";
import { ReviewComparisonInputs, SavedComparisonInputs, type ComparisonSelection } from "@/workflow-comparison-selection";

type PlanRevisionSource = { sourceAttemptId: string; proposalSequence: string; expectedPlanRevision: string | null };

export function DeliveryAgent({ scope, nodeId, inputs, unitId = null, taskId, job, label, actions, openActivity, planRevision, pinnedModel }: {
  scope: DeliveryTarget;
  nodeId: DeliveryNode;
  inputs: DeliveryInputs;
  unitId?: string | null;
  taskId?: string;
  job?: Job;
  label: string;
  actions: DeliveryActions;
  openActivity: WorkspaceProps["openActivity"];
  planRevision?: PlanRevisionSource;
  pinnedModel?: DeliveryModelChoice;
}) {
  const [selection, setSelection] = useState<DeliveryModelChoice>(null);
  const [revisionSource, setRevisionSource] = useState<PlanRevisionSource>();
  const [feedback, setFeedback] = useState("");
  const [comparison, setComparison] = useState<ComparisonSelection>();
  const attempt = job?.latestAttempt;
  const active = Boolean(job && (
    (!attempt && job.queueState === "queued") ||
    (attempt && ["starting", "running", "waiting_for_input"].includes(attempt.status))
  ));
  const revising = Boolean(revisionSource && planRevision && attempt?.status === "succeeded");
  const choosing = !active && (attempt?.status !== "succeeded" || revising);
  const { options, loading: loadingOptions } = useLiveModelOptions(deliveryProviders, choosing);
  const role = nodeId === "implement" ? "implementer" : nodeId === "review-candidate" ? "candidate reviewer" : undefined;
  const assignments = useTaskAssignments(scope.projectId, taskId, !pinnedModel && Boolean(role) && !active && attempt?.status !== "succeeded");
  const planned = assignments.data?.find((assignment) => assignment.role === role);
  const defaults = useProjectModelDefaults(scope.projectId, !pinnedModel && choosing && (nodeId === "plan" || nodeId === "implement"));
  const projectDefault = nodeId === "plan" ? defaults.data?.planning : nodeId === "implement" ? defaults.data?.implementation : undefined;
  const plannedSelection = planned?.modelId ? {
    provider: planned.provider, profileId: planned.profileId, modelId: planned.modelId,
  } : null;
  const preferred = pinnedModel ?? selection ?? plannedSelection ?? projectDefault;
  const effectiveSelection = availableModelSelection(options, preferred);
  const pinnedOption = pinnedModel && options.find(({ selection }) => sameModelSelection(selection, pinnedModel));
  const ready = effectiveSelection !== null;
  return <section className="pipeline-stack" aria-label={label}>
    <h3 className="text-sm font-medium">{label}</h3>
    {nodeId === "plan" && <p className="text-xs text-muted-foreground">Plan Mode</p>}
    {job && <div className="pipeline-actions">
      <span role="status">{attempt?.status === "waiting_for_input" ? "Waiting for input" :
        (attempt?.status ?? job.queueState).replaceAll("_", " ")}</span>
      {attempt && <Button variant="outline" onClick={() => openActivity(attempt.runId, attempt.conversationId ?? undefined)}>
        Open {label.toLowerCase()} attempt
      </Button>}
    </div>}
    {job && nodeId === "review-plan" && <SavedComparisonInputs key={job.id} projectId={scope.projectId} jobId={job.id} openActivity={openActivity} />}
    {planRevision && !active && attempt?.status === "succeeded" && !revising &&
      <Button type="button" variant="outline" disabled={actions.busy}
        onClick={() => setRevisionSource(planRevision)}>Request plan revision</Button>}
    {choosing && <>
      <ModelPicker label={`${label} model`} options={options}
        loading={loadingOptions} disabled={Boolean(pinnedModel) || actions.busy}
        selection={{ mode: "single", selected: pinnedModel ?? effectiveSelection, onChange: setSelection }} />
      {pinnedModel && <div className="text-sm text-muted-foreground">
        <p>{ready ? "Pinned stage model" : `Pinned stage model · ${pinnedModel.modelId} · ${loadingOptions ? "Checking availability" : "Unavailable in current profiles"}`}</p>
        {pinnedOption && <ModelRatingMeters rating={pinnedOption.rating} />}
      </div>}
      {!pinnedModel && !selection && planned?.modelId && <p className="text-sm text-muted-foreground">
        {ready ? "Model from task assignment" : `Task assignment · ${planned.modelId} · ${loadingOptions ? "Checking model availability" : "Unavailable in current profiles"}`}
      </p>}
      {!pinnedModel && !selection && !plannedSelection && projectDefault && <p className="text-sm text-muted-foreground">
        {ready ? "Model from project defaults" : `Project model · ${projectDefault.modelId} · ${loadingOptions ? "Checking availability" : "Unavailable in current profiles"}`}
      </p>}
      {assignments.error && <p role="alert">{String(assignments.error)}</p>}
      {nodeId === "review-plan" && <ReviewComparisonInputs projectId={scope.projectId} selection={comparison} onChange={setComparison} />}
      {revising && revisionSource ? <div className="pipeline-form pipeline-stack">
        <label className="pipeline-stack">
          Feedback for proposal {revisionSource.proposalSequence}
          <textarea rows={4} value={feedback} onChange={(event) => setFeedback(event.currentTarget.value)} />
        </label>
        <div className="pipeline-actions">
          <Button type="button" disabled={!ready || !feedback.trim() || actions.busy}
            onClick={() => actions.execute(async () => {
              const queued = await invoke("request_repository_plan_revision", { input: {
                ...scope, contextId: inputs.context, ...revisionSource,
                selection: effectiveSelection, effort: null, feedback,
              } });
              setRevisionSource(undefined);
              setFeedback("");
              return queued;
            })}>Queue plan revision</Button>
          <Button type="button" variant="outline" disabled={actions.busy}
            onClick={() => { setRevisionSource(undefined); setFeedback(""); }}>Cancel revision request</Button>
        </div>
      </div> : <Button type="button" disabled={!ready || actions.busy}
        onClick={() => actions.execute(() => invoke("submit_repository_delivery_agent", {
          input: { ...scope, nodeId, unitId, inputs, selection: effectiveSelection, effort: null,
            ...(nodeId === "review-plan" && comparison ? { comparison } : {}),
          },
        }))}>
        {job ? "Queue another attempt" : `Queue ${label.toLowerCase()}`}
      </Button>}
    </>}
  </section>;
}
