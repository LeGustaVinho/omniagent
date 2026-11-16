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
import type { WorkspaceProps } from "@/workbench-contract";

export function DeliveryAgent({ scope, nodeId, inputs, unitId = null, job, label, actions, openActivity }: {
  scope: DeliveryTarget;
  nodeId: DeliveryNode;
  inputs: DeliveryInputs;
  unitId?: string | null;
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
  const ready = selection && options.some(({ selection: option, selectable }) => selectable &&
    option.provider === selection.provider && option.profileId === selection.profileId && option.modelId === selection.modelId);
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
        loading={models.some(({ isFetching }) => isFetching)} disabled={actions.busy}
        selection={{ mode: "single", selected: selection, onChange: setSelection }} />
      <Button type="button" disabled={!ready || actions.busy}
        onClick={() => actions.execute(() => invoke("submit_repository_delivery_agent", {
          input: { ...scope, nodeId, unitId, inputs, selection, effort: null },
        }))}>
        {job ? "Queue another attempt" : `Queue ${label.toLowerCase()}`}
      </Button>
    </>}
  </section>;
}
