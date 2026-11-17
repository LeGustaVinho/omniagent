import type { ModelSelection } from "@/bindings/ModelSelection";
import type { SessionMode } from "@/bindings/SessionMode";
import { ModelPicker } from "@/components/model-picker";
import { sameModelSelection, useLiveModelOptions } from "@/live-models";
import type { WorkflowNodeConfiguration } from "@/workflow-graph-model";
import { WorkflowRequirementsEditor, type RequirementConfiguration } from "@/workflow-stage-requirements";
import { readStageGoal, WorkflowStageGoal, type StageGoalConfiguration, type StageGoalRoleRevision } from "@/workflow-stage-goal";
import { stagePlanMode, WorkflowStagePlan } from "@/workflow-stage-plan";

export function AgentRoleFields({ projectId, checkoutId, mode, configuration, nodeId, nodeKind, goalRole, disabled, fixedModel = false, onChange }: {
  projectId: string;
  checkoutId?: string;
  mode: SessionMode;
  configuration: WorkflowNodeConfiguration;
  nodeId: string;
  nodeKind: string;
  goalRole?: { version?: StageGoalRoleRevision; pending?: boolean };
  disabled: boolean;
  fixedModel?: boolean;
  onChange: (configuration: WorkflowNodeConfiguration) => void;
}) {
  const { options, loading, errors } = useLiveModelOptions();
  const model = configuration.model as ModelSelection | undefined;
  const requirements = (configuration.requirements as (RequirementConfiguration & {
    goal?: StageGoalConfiguration; boundRole?: StageGoalRoleRevision; reviewReuse?: boolean;
  }) | undefined) ?? {};
  const generic = nodeKind === "agent";
  const reviewReuse = requirements.reviewReuse === true;
  const planModeEnabled = generic && Boolean(requirements.plan);
  const effectiveMode = nodeKind === "plan" || planModeEnabled ? stagePlanMode(requirements.plan) : mode;
  const goal = readStageGoal(requirements.goal);
  const goalRoleVersion = goalRole?.version ?? requirements.boundRole;
  const previewConfiguration = goalRoleVersion
    ? { ...configuration, requirements: { ...requirements, boundRole: goalRoleVersion } } : configuration;
  const { model: _model, ...inherited } = configuration;
  const unavailable = model && !loading && !options.some((entry) =>
    entry.selectable && sameModelSelection(entry.selection, model));
  return <>
    <label>Agent role <input value={typeof configuration.role === "string" ? configuration.role : ""}
      required disabled={disabled} onChange={(event) => onChange({ ...configuration, role: event.currentTarget.value })} /></label>
    <ModelPicker label="Stage model override" options={options} loading={loading} disabled={disabled || fixedModel}
      selection={{ mode: "single", selected: model ?? null, onChange: (selection) =>
        onChange({ ...configuration, model: selection }),
      reset: { label: "Use runtime model defaults", onSelect: () => onChange(inherited) } }} />
    {unavailable && <p className="text-sm text-muted-foreground">Configured stage model · {model.modelId} · Unavailable in current profiles</p>}
    {errors[0] && <p role="alert">{String(errors[0])}</p>}
    {generic && <label className="workflow-check">
      <input type="checkbox" checked={reviewReuse}
        disabled={disabled || fixedModel || (!reviewReuse && Boolean(requirements.goal || requirements.plan))}
        onChange={(event) => {
          const { requirements: _previous, ...node } = configuration;
          const { reviewReuse: _before, ...rest } = requirements;
          if (event.currentTarget.checked) onChange({ ...node, requirements: { ...rest, reviewReuse: true } });
          else onChange(Object.keys(rest).length ? { ...node, requirements: rest } : node);
        }} />
      Reuse approved reviews when their evidence is unchanged
    </label>}
    {(nodeKind === "plan" || generic) && <WorkflowStagePlan plan={requirements.plan} optional={generic}
      goalEnabled={generic && Boolean(requirements.goal)} disabled={disabled || fixedModel || reviewReuse}
      onChange={(plan) => {
        const { requirements: _previous, ...node } = configuration;
        const { plan: _before, ...rest } = requirements;
        if (plan) onChange({ ...node, requirements: { ...rest, plan } });
        else onChange(Object.keys(rest).length ? { ...node, requirements: rest } : node);
      }} />}
    <WorkflowRequirementsEditor projectId={projectId} checkoutId={checkoutId} mode={effectiveMode} model={model ?? null}
      options={options} disabled={disabled || fixedModel} rolePin configuration={requirements}
      onChange={(requirements) => onChange({ ...configuration, requirements })} />
    <WorkflowStageGoal projectId={projectId} checkoutId={checkoutId} mode={effectiveMode} model={model ?? null}
      node={{ id: nodeId, kind: nodeKind, configuration: previewConfiguration }} roleRevision={goalRoleVersion}
      roleReferencePending={goalRole?.pending} disabled={disabled || fixedModel || reviewReuse} goal={goal} planModeEnabled={planModeEnabled}
      malformed={requirements.goal !== undefined && !goal}
      onChange={(goal) => {
        const { requirements: _previous, ...node } = configuration;
        const { goal: _before, ...rest } = requirements;
        if (goal) onChange({ ...node, requirements: { ...rest, goal } });
        else onChange(Object.keys(rest).length ? { ...node, requirements: rest } : node);
      }} />
  </>;
}
