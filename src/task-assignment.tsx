import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import { ModelPicker, ModelRatingMeters } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { sameModelSelection, useLiveModelOptions } from "@/live-models";

type Scope = { projectId: string; taskId: string };
type Assignment = { role: string; provider: Provider; profileId: string; modelId: string | null };

const roles = new Map([
  ["planner", "Planning"],
  ["plan reviewer", "Plan review"],
  ["implementer", "Implementation"],
  ["candidate reviewer", "Candidate review"],
]);

const providerNames: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Antigravity" };

function assignmentKey(projectId: string, taskId: string | undefined) {
  return ["project-tasks", projectId, taskId, "assignments"];
}

export function useTaskAssignments(projectId: string, taskId: string | undefined, enabled: boolean) {
  return useQuery({
    queryKey: assignmentKey(projectId, taskId),
    queryFn: () => invoke<Assignment[]>("project_task_assignments", { projectId, taskId }),
    enabled: enabled && Boolean(taskId),
  });
}

function ModelAssignment({ assignment, disabled, save }: {
  assignment?: Assignment;
  disabled: boolean;
  save: (selection: ModelSelection | null) => void;
}) {
  const [requestedProvider, setProvider] = useState<Provider>();
  const provider = requestedProvider ?? assignment?.provider ?? "codex";
  const { options, loading, errors } = useLiveModelOptions([provider]);
  const selected = assignment?.provider === provider && assignment.modelId ? {
    provider,
    profileId: assignment.profileId,
    modelId: assignment.modelId,
  } : null;
  const selectedOption = options.find(({ selection }) => sameModelSelection(selection, selected));
  return <>
    <label>Agent provider <select value={provider} disabled={disabled}
      onChange={(event) => setProvider(event.currentTarget.value as Provider)}>
      {Object.entries(providerNames).map(([key, name]) => <option key={key} value={key}>{name}</option>)}
    </select></label>
    <ModelPicker label="Assignment model" options={options} loading={loading}
      disabled={disabled || (loading && !assignment)}
      selection={{ mode: "single", selected, onChange: save }} />
    {errors[0] && <p role="alert">{String(errors[0])}</p>}
    {assignment && <div className="grid gap-1">
      <p className="break-words">Saved assignment · {providerNames[assignment.provider]} · {assignment.modelId ?? "Profile default"} · {assignment.profileId}</p>
      {selectedOption && <div role="group" aria-label={`Reference rating for ${selectedOption.name}`}>
        <ModelRatingMeters rating={selectedOption.rating} />
      </div>}
      {selectedOption && <p className="text-sm">Model availability · {selectedOption.selectable ? "Reported by CLI" : selectedOption.unavailableReason ?? "Unavailable"}</p>}
      <p className="text-sm">Verification status · Per run</p>
      <Button type="button" variant="outline" className="justify-self-start" disabled={disabled}
        onClick={() => save(null)}>Clear assignment</Button>
    </div>}
  </>;
}

export function TaskAssignment(scope: Scope) {
  const [expanded, setExpanded] = useState(false);
  const [role, setRole] = useState("implementer");
  const client = useQueryClient();
  const assignments = useTaskAssignments(scope.projectId, scope.taskId, expanded);
  const save = useMutation({
    mutationFn: (selection: ModelSelection | null) =>
      invoke<Assignment[]>("set_project_task_assignment", { ...scope, role, selection }),
    onSuccess: (saved) => client.setQueryData(assignmentKey(scope.projectId, scope.taskId), saved),
  });
  const declaredRoles = new Map(roles);
  for (const assignment of assignments.data ?? []) {
    if (!declaredRoles.has(assignment.role)) declaredRoles.set(assignment.role, assignment.role);
  }
  return <details className="pipeline-form" onToggle={(event) => setExpanded(event.currentTarget.open)}>
    <summary>Planned model assignments</summary>
    {expanded && <div className="pipeline-stack mt-3">
      {assignments.isPending && <p role="status">Loading model assignments…</p>}
      {assignments.error && <p role="alert">{String(assignments.error)}</p>}
      {assignments.data && <>
        <label>Workflow role <select value={role} disabled={save.isPending} onChange={(event) => {
          setRole(event.currentTarget.value);
          save.reset();
        }}>
          {Array.from(declaredRoles, ([id, label]) => <option key={id} value={id}>{label}</option>)}
        </select></label>
        <ModelAssignment key={role} assignment={assignments.data.find((assignment) => assignment.role === role)}
          disabled={save.isPending} save={(selection) => save.mutate(selection)} />
      </>}
      {save.isSuccess && <span role="status">Assignment saved</span>}
      {save.error && <p role="alert">{String(save.error)}</p>}
    </div>}
  </details>;
}
