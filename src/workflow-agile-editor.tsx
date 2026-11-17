import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { workflowStates, type Hierarchy, type WorkflowStatus } from "@/project-agile-model";
import type { WorkflowNodeConfiguration } from "@/workflow-graph-model";

type FindWork = {
  kind: "work" | "task" | "subtask" | "checklist";
  result: "list" | "single";
  query?: string;
  state?: WorkflowStatus;
  checked?: boolean;
  parentTaskId?: string;
};

type Progress = { kind: "checklist"; checked: boolean } | { kind: "transition"; next: WorkflowStatus };
type ManageWork = "link" | "create_task" | "create_subtask";
type WorkContextMode = "work" | "markdown" | "both";

type FieldsProps = {
  projectId: string;
  configuration: WorkflowNodeConfiguration;
  onChange: (configuration: WorkflowNodeConfiguration) => void;
  disabled: boolean;
};

export function WorkflowAgileFields(props: FieldsProps) {
  switch (props.configuration.kind) {
    case "find_work": return <FindWorkFields {...props} />;
    case "update_progress": return <ProgressFields {...props} />;
    case "manage_work": return <ManageWorkFields {...props} />;
    case "load_work_context": return <WorkContextFields {...props} />;
    default: return null;
  }
}

function FindWorkFields({ projectId, configuration, onChange, disabled }: FieldsProps) {
  const filters = (configuration.query ?? { kind: "work", result: "list" }) as FindWork;
  const hierarchy = useQuery({
    queryKey: ["project-tasks", projectId, "hierarchy"],
    queryFn: () => invoke<Hierarchy>("project_work_hierarchy", { projectId }),
    enabled: filters.kind !== "task",
  });
  const parents = hierarchy.data?.work.filter(({ placement }) => placement.kind === "task") ?? [];
  const missingParent = filters.parentTaskId && !parents.some(({ task }) => task.id === filters.parentTaskId);
  function update(patch: Partial<FindWork>) {
    onChange({ kind: "find_work", query: { ...filters, ...patch } });
  }

  return <>
    <label>Work to find<select disabled={disabled} value={filters.kind} onChange={(event) => {
      const kind = event.currentTarget.value as FindWork["kind"];
      update({ kind, checked: kind === "checklist" ? filters.checked : undefined,
        parentTaskId: kind === "task" ? undefined : filters.parentTaskId });
    }}>
      <option value="work">Tasks and Sub-Tasks</option>
      <option value="task">Tasks</option>
      <option value="subtask">Sub-Tasks</option>
      <option value="checklist">Checklist items</option>
    </select></label>
    <label>{filters.kind === "checklist" ? "Checklist text contains" : "Title contains"}
      <input type="search" value={filters.query ?? ""} disabled={disabled} maxLength={4096}
        onChange={(event) => update({ query: event.currentTarget.value || undefined })} />
    </label>
    <label>Work status<select disabled={disabled} value={filters.state ?? ""}
      onChange={(event) => update({ state: (event.currentTarget.value || undefined) as WorkflowStatus | undefined })}>
      <option value="">Any status</option>
      {workflowStates.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}
    </select></label>
    {filters.kind !== "task" && <label>Parent Task<select disabled={disabled || hierarchy.isPending}
      value={filters.parentTaskId ?? ""} onChange={(event) => update({ parentTaskId: event.currentTarget.value || undefined })}>
      <option value="">Any parent</option>
      {missingParent && <option value={filters.parentTaskId}>Unavailable parent</option>}
      {parents.map(({ task }) => <option key={task.id} value={task.id}>{task.title}</option>)}
    </select></label>}
    {hierarchy.isError && filters.kind !== "task" && <p role="alert">{String(hierarchy.error)}</p>}
    {filters.kind === "checklist" && <label>Checklist state<select disabled={disabled}
      value={filters.checked === undefined ? "" : String(filters.checked)} onChange={(event) => {
        const selected = event.currentTarget.value;
        update({ checked: selected ? selected === "true" : undefined });
      }}>
      <option value="">Any checklist state</option>
      <option value="false">Unchecked</option>
      <option value="true">Checked</option>
    </select></label>}
    <label>Results<select value={filters.result} disabled={disabled}
      onChange={(event) => update({ result: event.currentTarget.value as FindWork["result"] })}>
      <option value="list">List of matches</option>
      <option value="single">Exactly one match</option>
    </select></label>
    <p className="text-xs text-muted-foreground">Exactly one match provides Task and progress target outputs.</p>
  </>;
}

function ProgressFields({ configuration, onChange, disabled }: FieldsProps) {
  const action = (configuration.action ?? { kind: "checklist", checked: true }) as Progress;
  const operation = action.kind === "transition" ? "transition" : action.checked ? "check" : "reopen";
  const update = (next: Progress) => onChange({ kind: "update_progress", action: next });
  return <>
    <label>Progress action<select disabled={disabled} value={operation} onChange={(event) => {
      const value = event.currentTarget.value;
      update(value === "transition" ? { kind: "transition", next: "ready" } : { kind: "checklist", checked: value === "check" });
    }}>
      <option value="check">Check item</option>
      <option value="reopen">Reopen item</option>
      <option value="transition">Change work status</option>
    </select></label>
    {action.kind === "transition" && <label>New work status<select disabled={disabled} value={action.next}
      onChange={(event) => update({ kind: "transition", next: event.currentTarget.value as WorkflowStatus })}>
      {workflowStates.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}
    </select></label>}
    <p className="text-xs text-muted-foreground">Connect a progress target with the expected work state or checklist revision.</p>
  </>;
}

function ManageWorkFields({ configuration, onChange, disabled }: FieldsProps) {
  const action = configuration.action as { kind?: ManageWork } | undefined;
  const kind = action?.kind ?? "link";
  const connectionHint = {
    link: "Connect the Task or Sub-Task to link.",
    create_task: "Connect a title from Build text.",
    create_subtask: "Connect the parent Task and a title from Build text.",
  }[kind];
  return <>
    <label>Work action<select disabled={disabled} value={kind} onChange={(event) => {
      onChange({ kind: "manage_work", action: { kind: event.currentTarget.value } });
    }}>
      <option value="link">Link Task or Sub-Task</option>
      <option value="create_task">Create Task</option>
      <option value="create_subtask">Create Sub-Task</option>
    </select></label>
    <p className="text-xs text-muted-foreground">{connectionHint}</p>
  </>;
}

function WorkContextFields({ configuration, onChange, disabled }: FieldsProps) {
  const mode = (configuration.mode ?? "work") as WorkContextMode;
  return <label>Context source<select disabled={disabled} value={mode} onChange={(event) => {
    onChange({ kind: "load_work_context", mode: event.currentTarget.value });
  }}>
    <option value="work">Task descriptions and checklist</option>
    <option value="markdown">Repository Markdown revision</option>
    <option value="both">Task and repository Markdown</option>
  </select></label>;
}
