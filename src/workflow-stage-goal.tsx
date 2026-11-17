import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { SessionMode } from "@/bindings/SessionMode";
import { Button } from "@/components/ui/button";
import type { WorkflowGraphNode } from "@/workflow-graph-model";

export type StageGoalEvidence = "operator_review" | "candidate_gate";
export type StageGoalCriterion = { text: string; evidence: StageGoalEvidence };
export type StageGoalConfiguration = {
  objective: string;
  criteria: StageGoalCriterion[];
  maxTurns?: number;
  maxSeconds?: number;
  tokenBudget?: number;
};
export type StageGoalRoleRevision = {
  kind: string; id: string; revision: string; destination: "library" | "project";
};

type StageGoalRoute =
  | { route: "native_candidate"; evidenceId: string }
  | { route: "managed" | "blocked"; reason: string };
type StageGoalAssessment = {
  pin: { stageRevision: string; configurationRevision: string; role: string; roleRevision?: StageGoalRoleRevision;
    configuration: StageGoalConfiguration };
  route: StageGoalRoute;
};
type RouteRequest = { projectId: string; checkoutId: string; node: WorkflowGraphNode; selection: ModelSelection };
type RoleArtifact = { name: string; body: { kind: string; template?: { model: ModelSelection } } };

const limits = [
  ["maxTurns", "Turn limit", 4_294_967_295],
  ["maxSeconds", "Time limit in seconds", 4_294_967_295],
  ["tokenBudget", "Token budget", Number.MAX_SAFE_INTEGER],
] as const;
const blankCriterion = (): StageGoalCriterion => ({ text: "", evidence: "operator_review" });

function goalProblem(goal: StageGoalConfiguration, mode: SessionMode) {
  if (!goal.objective.trim()) return "Enter a goal objective.";
  if (!goal.criteria.length || goal.criteria.some(({ text }) => !text.trim())) {
    return "Describe every criterion and select its required evidence.";
  }
  if (mode !== "approved_write" && goal.criteria.some(({ evidence }) => evidence === "candidate_gate")) {
    return "Candidate gate evidence requires a writing stage.";
  }
  if (limits.every(([key]) => goal[key] === undefined)) return "Set at least one execution limit.";
  for (const [key, label, max] of limits) {
    const value = goal[key];
    if (value !== undefined && (!Number.isSafeInteger(value) || value <= 0 || value > max)) {
      return `${label} must be a positive integer no greater than ${max}.`;
    }
  }
  return undefined;
}

export function readStageGoal(value: unknown): StageGoalConfiguration | undefined {
  if (!value || typeof value !== "object") return undefined;
  const goal = value as Partial<StageGoalConfiguration>;
  if (typeof goal.objective !== "string" || !Array.isArray(goal.criteria) || goal.criteria.some((criterion) =>
    !criterion || typeof criterion.text !== "string" ||
    (criterion.evidence !== "operator_review" && criterion.evidence !== "candidate_gate"))) return undefined;
  if (limits.some(([key]) => goal[key] !== undefined && typeof goal[key] !== "number")) return undefined;
  return goal as StageGoalConfiguration;
}

function Result({ assessment }: { assessment: StageGoalAssessment }) {
  const { pin, route } = assessment;
  return <div className="pipeline-evidence pipeline-stack" aria-label="Goal route assessment">
    <p>Stage role · {pin.role}{pin.roleRevision && <> · <code className="break-all">{pin.roleRevision.revision}</code></>}</p>
    {route.route === "native_candidate" ? <>
      <p role="status">Qualified native Goal candidate</p>
      <p>Qualification evidence <code className="break-all">{route.evidenceId}</code></p>
    </> : <p role={route.route === "blocked" ? "alert" : "status"}>
      {route.route === "managed" ? "OmniAgent-managed continuation" : "Goal Mode blocked"} · {route.reason}
    </p>}
    <p>Dispatch checks the effective model, role and worktree again.</p>
  </div>;
}

export function WorkflowStageGoal({ projectId, checkoutId, node, mode, model, roleRevision, roleReferencePending,
  disabled, goal, malformed, planModeEnabled, onChange }: {
  projectId: string; checkoutId?: string; node: WorkflowGraphNode; mode: SessionMode; model: ModelSelection | null;
  roleRevision?: StageGoalRoleRevision; roleReferencePending?: boolean;
  disabled: boolean; goal?: StageGoalConfiguration; malformed?: boolean; planModeEnabled?: boolean;
  onChange: (next: StageGoalConfiguration | undefined) => void;
}) {
  const [open, setOpen] = useState(false);
  const role = useQuery({
    queryKey: ["workflow-stage-goal-role", projectId, roleRevision],
    queryFn: () => invoke<RoleArtifact | null>("read_published_workflow_package_artifact", { projectId, version: roleRevision }),
    enabled: open && Boolean(goal && roleRevision && !model),
    retry: false,
  });
  const selection = model ?? role.data?.body.template?.model ?? null;
  const unsupported = planModeEnabled ? "Turn off Plan Mode to use Goal Mode."
    : node.kind === "review_candidate" ? "Candidate review runs in an isolated checkout without the Goal bridge." : undefined;
  const request = checkoutId && goal && selection ? { projectId, checkoutId, node, selection } : undefined;
  const preview = useMutation({
    mutationFn: (args: RouteRequest) => invoke<StageGoalAssessment | null>("preview_workflow_stage_goal", args),
  });
  const current = request && JSON.stringify(preview.variables) === JSON.stringify(request);
  const issue = goal && goalProblem(goal, mode);
  const roleName = typeof node.configuration.role === "string" ? node.configuration.role.trim() : "";
  const selectable = !disabled && !unsupported;
  const canPreview = request && !issue && roleName && !unsupported && !roleReferencePending && !preview.isPending;

  function updateCriterion(index: number, patch: Partial<StageGoalCriterion>) {
    if (goal) onChange({ ...goal, criteria: goal.criteria.map((criterion, ordinal) =>
      index === ordinal ? { ...criterion, ...patch } : criterion) });
  }

  return <details className="pipeline-evidence" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Goal Mode{goal && " · enabled"}</summary>
    {open && <div className="pipeline-stack">
      <label><input type="checkbox" checked={Boolean(goal) || Boolean(planModeEnabled && malformed)}
        disabled={disabled || Boolean(unsupported && !goal && !malformed)} onChange={(event) => {
          if (!event.currentTarget.checked) onChange(undefined);
          else if (!planModeEnabled) onChange({ objective: "", criteria: [blankCriterion()] });
        }} /> Use Goal Mode for this stage</label>
      {unsupported && <p role="alert">{unsupported}</p>}
      {malformed && <p role="alert">The saved goal settings are incomplete. Review the stage configuration.</p>}
      {goal && <>
        {roleName && <p>Goal role · {roleName}{roleRevision && <> · <code className="break-all">{roleRevision.revision}</code></>}</p>}
        <fieldset className="pipeline-stack" disabled={!selectable}>
          <label>Goal objective<textarea rows={3} value={goal.objective}
            onChange={(event) => onChange({ ...goal, objective: event.currentTarget.value })} /></label>
          <fieldset className="pipeline-stack">
            <legend>Required goal evidence</legend>
            {goal.criteria.map((criterion, index) => <div key={index} className="pipeline-stack">
              <label>Criterion {index + 1}<input value={criterion.text}
                onChange={(event) => updateCriterion(index, { text: event.currentTarget.value })} /></label>
              <label>Evidence for criterion {index + 1}<select value={criterion.evidence} onChange={(event) =>
                updateCriterion(index, { evidence: event.currentTarget.value as StageGoalEvidence })}>
                <option value="operator_review">Operator review</option>
                {(mode === "approved_write" || criterion.evidence === "candidate_gate") &&
                  <option value="candidate_gate" disabled={mode !== "approved_write"}>Candidate gate result</option>}
              </select></label>
              <Button type="button" variant="outline" aria-label={`Remove criterion ${index + 1}`}
                onClick={() => onChange({ ...goal, criteria: goal.criteria.filter((_, ordinal) => index !== ordinal) })}>Remove criterion</Button>
            </div>)}
            <Button type="button" variant="outline"
              onClick={() => onChange({ ...goal, criteria: [...goal.criteria, blankCriterion()] })}>Add criterion</Button>
          </fieldset>
          <fieldset className="pipeline-stack">
            <legend>Goal limits</legend>
            {limits.map(([key, label, max]) => <label key={key}>{label}
              <input type="number" min={1} max={max} step={1} value={goal[key] ?? ""} onChange={(event) => {
                const next = { ...goal };
                if (event.currentTarget.value === "") delete next[key];
                else next[key] = event.currentTarget.valueAsNumber;
                onChange(next);
              }} />
            </label>)}
          </fieldset>
        </fieldset>
        {issue && <p role="status">{issue}</p>}
        {roleReferencePending && <p>Publish the referenced role before evaluating this stage.</p>}
        {role.error && <p role="alert">{String(role.error)}</p>}
        {!role.error && !roleReferencePending && !selection && <p>{roleRevision && role.isPending
          ? "Loading the pinned role model…" : "Choose a stage model to evaluate the Goal route."}</p>}
        {!checkoutId && <p>Select a checkout to evaluate the Goal route.</p>}
        <Button type="button" variant="outline" disabled={!canPreview} onClick={() => { if (canPreview) preview.mutate(request); }}>
          {preview.isPending ? "Checking Goal route…" : "Check Goal route"}
        </Button>
        {current && preview.error && <p role="alert">{String(preview.error)}</p>}
        {current && preview.data && <Result assessment={preview.data} />}
      </>}
    </div>}
  </details>;
}
