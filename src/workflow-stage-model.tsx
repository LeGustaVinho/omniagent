import type { ModelSelection } from "@/bindings/ModelSelection";
import type { SessionMode } from "@/bindings/SessionMode";
import { ModelPicker } from "@/components/model-picker";
import { sameModelSelection, useLiveModelOptions } from "@/live-models";
import type { WorkflowNodeConfiguration } from "@/workflow-graph-model";
import { WorkflowRequirementsEditor, type RequirementConfiguration } from "@/workflow-stage-requirements";

export function AgentRoleFields({ projectId, checkoutId, mode, configuration, disabled, onChange }: {
  projectId: string;
  checkoutId?: string;
  mode: SessionMode;
  configuration: WorkflowNodeConfiguration;
  disabled: boolean;
  onChange: (configuration: WorkflowNodeConfiguration) => void;
}) {
  const { options, loading, errors } = useLiveModelOptions();
  const model = configuration.model as ModelSelection | undefined;
  const { model: _model, ...inherited } = configuration;
  const unavailable = model && !loading && !options.some((entry) =>
    entry.selectable && sameModelSelection(entry.selection, model));
  return <>
    <label>Agent role <input value={typeof configuration.role === "string" ? configuration.role : ""}
      required disabled={disabled} onChange={(event) => onChange({ ...configuration, role: event.currentTarget.value })} /></label>
    <ModelPicker label="Stage model override" options={options} loading={loading} disabled={disabled}
      selection={{ mode: "single", selected: model ?? null, onChange: (selection) =>
        onChange({ ...configuration, model: selection }),
      reset: { label: "Use runtime model defaults", onSelect: () => onChange(inherited) } }} />
    {unavailable && <p className="text-sm text-muted-foreground">Configured stage model · {model.modelId} · Unavailable in current profiles</p>}
    {errors[0] && <p role="alert">{String(errors[0])}</p>}
    <WorkflowRequirementsEditor projectId={projectId} checkoutId={checkoutId} mode={mode} model={model ?? null}
      options={options} disabled={disabled} rolePin configuration={(configuration.requirements as RequirementConfiguration | undefined) ?? {}}
      onChange={(requirements) => onChange({ ...configuration, requirements })} />
  </>;
}
