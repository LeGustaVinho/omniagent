import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Job } from "@/bindings/Job";
import { Button } from "@/components/ui/button";
import { ModelPicker, ModelRatingMeters } from "@/components/model-picker";
import { availableModelSelection, sameModelSelection, useLiveModelOptions } from "@/live-models";
import {
  deliveryProviders, type DeliveryActions, type DeliveryInputs,
  type DeliveryModelChoice, type DeliveryTarget,
} from "@/repository-delivery-model";
import type { WorkspaceProps } from "@/workbench-contract";

export function DeliveryAgent({ scope, nodeId, inputs, unitId = null, job, label, actions,
  openActivity, pinnedModel, pinnedEffort = null }: {
  scope: DeliveryTarget;
  nodeId: string;
  inputs: DeliveryInputs;
  unitId?: string | null;
  job?: Job;
  label: string;
  actions: DeliveryActions;
  openActivity: WorkspaceProps["openActivity"];
  pinnedModel?: DeliveryModelChoice;
  pinnedEffort?: string | null;
}) {
  const [selection, setSelection] = useState<DeliveryModelChoice>(null);
  const attempt = job?.latestAttempt;
  const active = Boolean(job && (
    (!attempt && job.queueState === "queued") ||
    (attempt && ["starting", "running", "waiting_for_input"].includes(attempt.status))
  ));
  const choosing = !active && attempt?.status !== "succeeded";
  const { options, loading: loadingOptions } = useLiveModelOptions(deliveryProviders, choosing);
  const effectiveSelection = availableModelSelection(options, pinnedModel ?? selection);
  const pinnedOption = pinnedModel && options.find(({ selection }) => sameModelSelection(selection, pinnedModel));
  const ready = effectiveSelection !== null;
  return <section className="pipeline-stack" aria-label={label}>
    <h3 className="text-sm font-medium">{label}</h3>
    {job && <div className="pipeline-actions">
      <span role="status">{attempt?.status === "waiting_for_input" ? "Waiting for input" :
        (attempt?.status ?? job.queueState).replaceAll("_", " ")}</span>
      {attempt && <Button variant="outline" onClick={() => openActivity(attempt.runId, attempt.conversationId ?? undefined)}>
        Open {label.toLowerCase()} attempt
      </Button>}
    </div>}
    {choosing && <>
      <ModelPicker label={`${label} model`} options={options}
        loading={loadingOptions} disabled={Boolean(pinnedModel) || actions.busy}
        selection={{ mode: "single", selected: pinnedModel ?? effectiveSelection, onChange: setSelection }} />
      {pinnedModel && <div className="text-sm text-muted-foreground">
        <p>{ready ? "Pinned stage model" : `Pinned stage model · ${pinnedModel.modelId} · ${loadingOptions ? "Checking availability" : "Unavailable in current profiles"}`}</p>
        {pinnedOption && <ModelRatingMeters rating={pinnedOption.rating} />}
      </div>}
      <Button type="button" disabled={!ready || actions.busy}
        onClick={() => actions.execute(() => invoke("submit_repository_delivery_agent", {
          input: { ...scope, nodeId, unitId, inputs, selection: effectiveSelection, effort: pinnedEffort },
        }))}>
        {job ? "Queue another attempt" : `Queue ${label.toLowerCase()}`}
      </Button>
    </>}
  </section>;
}
