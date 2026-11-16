import { useState } from "react";
import { useQueries } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { Job } from "@/bindings/Job";
import { Button } from "@/components/ui/button";
import { ModelPicker } from "@/components/model-picker";
import {
  deliveryProviders, type DeliveryActions, type DeliveryInputs, type DeliveryModel,
  type DeliveryModelChoice, type DeliveryNode, type DeliveryTarget,
} from "@/repository-delivery-model";
import { useTaskAssignments } from "@/task-assignment";
import type { WorkspaceProps } from "@/workbench-contract";

export function DeliveryAgent({ scope, nodeId, inputs, unitId = null, taskId, job, label, actions, openActivity }: {
  scope: DeliveryTarget;
  nodeId: DeliveryNode;
  inputs: DeliveryInputs;
  unitId?: string | null;
  taskId?: string;
  job?: Job;
  label: string;
  actions: DeliveryActions;
  openActivity: WorkspaceProps["openActivity"];
}) {
  const [selection, setSelection] = useState<DeliveryModelChoice>(null);
  const attempt = job?.latestAttempt;
  const active = Boolean(job && (
    (!attempt && job.queueState === "queued") ||
    (attempt && ["starting", "running", "waiting_for_input"].includes(attempt.status))
  ));
  const models = useQueries({ queries: deliveryProviders.map((provider) => ({
    queryKey: ["repository-delivery-models", provider],
    queryFn: () => invoke<DeliveryModel[]>("conversation_models", { provider }),
    enabled: !active && attempt?.status !== "succeeded",
    retry: false,
    staleTime: 60_000,
  })) });
  const options = models.flatMap(({ data }) => data?.map(({ option }) => option) ?? []);
  const loadingOptions = models.some(({ isFetching }) => isFetching);
  const role = nodeId === "implement" ? "implementer" : nodeId === "review-candidate" ? "candidate reviewer" : undefined;
  const assignments = useTaskAssignments(scope.projectId, taskId, Boolean(role) && !active && attempt?.status !== "succeeded");
  const planned = assignments.data?.find((assignment) => assignment.role === role);
  const effectiveSelection = selection ?? (planned?.modelId ? {
    provider: planned.provider, profileId: planned.profileId, modelId: planned.modelId,
  } : null);
  const ready = effectiveSelection && options.some(({ selection: option, selectable }) => selectable &&
    option.provider === effectiveSelection.provider && option.profileId === effectiveSelection.profileId && option.modelId === effectiveSelection.modelId);
  return <section className="pipeline-stack" aria-label={label}>
    <h3 className="text-sm font-medium">{label}</h3>
    {job && <div className="pipeline-actions">
      <span role="status">{(attempt?.status ?? job.queueState).replaceAll("_", " ")}</span>
      {attempt && <Button variant="outline" onClick={() => openActivity(attempt.runId, attempt.conversationId ?? undefined)}>
        Open {label.toLowerCase()} attempt
      </Button>}
    </div>}
    {!active && attempt?.status !== "succeeded" && <>
      <ModelPicker label={`${label} model`} options={options}
        loading={loadingOptions} disabled={actions.busy}
        selection={{ mode: "single", selected: effectiveSelection, onChange: setSelection }} />
      {!selection && planned?.modelId && <p className="text-sm text-muted-foreground">
        {ready ? "Model from task assignment" : `Task assignment · ${planned.modelId} · ${loadingOptions ? "Checking model availability" : "Unavailable in current profiles"}`}
      </p>}
      {assignments.error && <p role="alert">{String(assignments.error)}</p>}
      <Button type="button" disabled={!ready || actions.busy}
        onClick={() => actions.execute(() => invoke("submit_repository_delivery_agent", {
          input: { ...scope, nodeId, unitId, inputs, selection: effectiveSelection, effort: null },
        }))}>
        {job ? "Queue another attempt" : `Queue ${label.toLowerCase()}`}
      </Button>
    </>}
  </section>;
}
