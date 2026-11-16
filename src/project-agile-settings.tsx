import { useState, type FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { Job } from "@/bindings/Job";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { Button } from "@/components/ui/button";
import { useTaskAssignments } from "@/task-assignment";
import type { Item, Work } from "@/project-agile-model";
import type { WorkspaceProps } from "@/workbench-contract";

type ChecklistMode = "track_progress" | "run_by_item";
type BranchStrategy = "independent" | "shared_branch_per_task";
type Scope = { projectId: string; taskId: string };
type WorkFields = { title: string; priority: number; acceptanceCriteria: string[]; prerequisites: string[]; blockers: string[] };
type TaskModeSettings = { overrideMode: ChecklistMode | null; effectiveMode: ChecklistMode };
type BranchSettings = { overrideStrategy: BranchStrategy | null; effectiveStrategy: BranchStrategy; locked: boolean };
type DispatchTarget = { workflowRunId: string; checkoutId: string; planRevision: string; releaseId: string; unitId: string };

const checklistLabels = { track_progress: "Track progress", run_by_item: "Run by item" } satisfies Record<ChecklistMode, string>;
const branchLabels = { independent: "Independent", shared_branch_per_task: "Shared branch per Task" } satisfies Record<BranchStrategy, string>;
const checklistOptions = Object.entries(checklistLabels).map(([value, label]) => <option key={value} value={value}>{label}</option>);
const branchOptions = Object.entries(branchLabels).map(([value, label]) => <option key={value} value={value}>{label}</option>);

export function ProjectWorkModes({ projectId }: { projectId: string }) {
  const [open, setOpen] = useState(false);
  return <details className="pipeline-form m-4" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary className="cursor-pointer font-medium">Project work modes</summary>
    {open && <ProjectWorkModeControls projectId={projectId} />}
  </details>;
}

function ProjectWorkModeControls({ projectId }: { projectId: string }) {
  const client = useQueryClient();
  const key = ["project-tasks", projectId, "work-modes"];
  const checklist = useQuery({ queryKey: [...key, "checklist"], queryFn: () => invoke<ChecklistMode>("project_checklist_mode", { projectId }) });
  const branch = useQuery({ queryKey: [...key, "branch"], queryFn: () => invoke<BranchStrategy>("project_branch_strategy", { projectId }) });
  const saveChecklist = useMutation({ mutationFn: (mode: ChecklistMode) => invoke("set_project_checklist_mode", { projectId, mode }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["project-tasks", projectId] }),
  });
  const saveBranch = useMutation({ mutationFn: (strategy: BranchStrategy) => invoke("set_project_branch_strategy", { projectId, strategy }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["project-tasks", projectId] }),
  });
  const error = checklist.error ?? branch.error ?? saveChecklist.error ?? saveBranch.error;
  return <div className="pipeline-actions mt-2">
    <label>Project checklist mode <select value={saveChecklist.isPending ? saveChecklist.variables : checklist.data ?? ""}
      disabled={!checklist.data || saveChecklist.isPending} onChange={(event) => saveChecklist.mutate(event.currentTarget.value as ChecklistMode)}>
      {!checklist.data && <option value="">Loading…</option>}{checklistOptions}
    </select></label>
    <label>Project branch strategy <select value={saveBranch.isPending ? saveBranch.variables : branch.data ?? ""}
      disabled={!branch.data || saveBranch.isPending} onChange={(event) => saveBranch.mutate(event.currentTarget.value as BranchStrategy)}>
      {!branch.data && <option value="">Loading…</option>}{branchOptions}
    </select></label>
    {error && <p role="alert">{String(error)}</p>}
  </div>;
}

export function TaskWorkSettings({ scope, workKind, candidates, items, openQueueJob }: {
  scope: Scope; workKind: "task" | "subtask"; candidates: Work[]; items: Item[]; openQueueJob?: WorkspaceProps["openQueueJob"];
}) {
  const [open, setOpen] = useState(false);
  const fields = useQuery({ queryKey: ["project-tasks", scope.projectId, scope.taskId, "fields"],
    queryFn: () => invoke<WorkFields>("project_work_fields", scope), enabled: open,
  });
  return <details className="pipeline-form" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary className="cursor-pointer font-medium">Work fields and execution</summary>
    {open && <div className="pipeline-stack mt-2">
      {fields.isPending && <p role="status">Loading work fields…</p>}
      {fields.error && <p role="alert">{String(fields.error)}</p>}
      {fields.data && <TaskWorkFields scope={scope} displayed={fields.data} candidates={candidates} reload={() => fields.refetch()} />}
      <TaskModeControls scope={scope} workKind={workKind} items={items} openQueueJob={openQueueJob} />
    </div>}
  </details>;
}

function lines(value: string) {
  return value.split("\n").map((line) => line.trim()).filter(Boolean);
}

function TaskWorkFields({ scope, displayed, candidates, reload }: { scope: Scope; displayed: WorkFields; candidates: Work[]; reload: () => void }) {
  const client = useQueryClient();
  const [base, setBase] = useState(displayed);
  const [title, setTitle] = useState(displayed.title);
  const [priority, setPriority] = useState(String(displayed.priority));
  const [criteria, setCriteria] = useState(displayed.acceptanceCriteria.join("\n"));
  const [prerequisites, setPrerequisites] = useState(displayed.prerequisites);
  const [blockers, setBlockers] = useState(displayed.blockers.join("\n"));
  const next = { title: title.trim(), priority: Number(priority), acceptanceCriteria: lines(criteria), prerequisites, blockers: lines(blockers) };
  const stale = JSON.stringify(displayed) !== JSON.stringify(base);
  const valid = Boolean(next.title && priority && Number.isInteger(next.priority) && next.priority >= -2147483648 && next.priority <= 2147483647);
  const save = useMutation({ mutationFn: () => invoke<WorkFields>("update_project_work_fields", { ...scope, expected: base, next }),
    onSuccess: (latest) => {
      apply(latest);
      client.setQueryData(["project-tasks", scope.projectId, scope.taskId, "fields"], latest);
      return client.invalidateQueries({ queryKey: ["project-tasks", scope.projectId] });
    },
    onError: reload,
  });
  function apply(fields: WorkFields) {
    setBase(fields); setTitle(fields.title); setPriority(String(fields.priority)); setCriteria(fields.acceptanceCriteria.join("\n"));
    setPrerequisites(fields.prerequisites); setBlockers(fields.blockers.join("\n"));
  }
  function submit(event: FormEvent) {
    event.preventDefault();
    if (!stale && valid && !save.isPending) save.mutate();
  }
  return <form className="pipeline-stack" aria-label="Direct work fields" onSubmit={submit}>
    <label>Work title <input required value={title} onChange={(event) => setTitle(event.currentTarget.value)} /></label>
    <label>Priority <input type="number" required value={priority} onChange={(event) => setPriority(event.currentTarget.value)} /></label>
    <label>Acceptance criteria <textarea rows={3} value={criteria} onChange={(event) => setCriteria(event.currentTarget.value)} /></label>
    <label>Blockers <textarea rows={2} value={blockers} onChange={(event) => setBlockers(event.currentTarget.value)} /></label>
    <label>Prerequisites <select multiple value={prerequisites} onChange={(event) => setPrerequisites(Array.from(event.currentTarget.selectedOptions, ({ value }) => value))}>
      {candidates.filter(({ id }) => id !== scope.taskId).map(({ id, title: label }) => <option key={id} value={id}>{label}</option>)}
    </select></label>
    {save.error && <p role="alert">{String(save.error)}</p>}
    {stale && <Button type="button" variant="outline" onClick={() => { apply(displayed); save.reset(); }}>Load latest work fields</Button>}
    <Button type="submit" className="justify-self-start" disabled={!valid || stale || save.isPending}>Save work fields</Button>
  </form>;
}

function TaskModeControls({ scope, workKind, items, openQueueJob }: {
  scope: Scope; workKind: "task" | "subtask"; items: Item[]; openQueueJob?: WorkspaceProps["openQueueJob"];
}) {
  const client = useQueryClient();
  const key = ["project-tasks", scope.projectId, scope.taskId];
  const checklist = useQuery({ queryKey: [...key, "checklist-mode"], queryFn: () => invoke<TaskModeSettings>("task_checklist_mode_settings", scope) });
  const branch = useQuery({ queryKey: [...key, "branch-strategy"], queryFn: () => invoke<BranchSettings>("task_branch_strategy", scope) });
  const saveChecklist = useMutation({ mutationFn: (mode: ChecklistMode | null) => invoke("set_task_checklist_mode", { ...scope, mode }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["project-tasks", scope.projectId] }),
  });
  const saveBranch = useMutation({ mutationFn: (strategy: BranchStrategy | null) => invoke("set_task_branch_strategy", { ...scope, strategy }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["project-tasks", scope.projectId] }),
  });
  const error = checklist.error ?? branch.error ?? saveChecklist.error ?? saveBranch.error;
  return <section className="pipeline-stack" aria-label="Work execution modes">
    <label>Checklist mode override <select value={(saveChecklist.isPending ? saveChecklist.variables : checklist.data?.overrideMode) ?? ""}
      disabled={!checklist.data || saveChecklist.isPending} onChange={(event) => saveChecklist.mutate(event.currentTarget.value as ChecklistMode || null)}>
      <option value="">Inherit {workKind === "task" ? "project mode" : "parent or project mode"}</option>{checklistOptions}
    </select></label>
    {checklist.data && <p>Effective checklist mode · {checklistLabels[checklist.data.effectiveMode]}</p>}
    {workKind === "task" && <label>Task branch strategy override <select value={(saveBranch.isPending ? saveBranch.variables : branch.data?.overrideStrategy) ?? ""}
      disabled={!branch.data || branch.data.locked || saveBranch.isPending} onChange={(event) => saveBranch.mutate(event.currentTarget.value as BranchStrategy || null)}>
      <option value="">Inherit project strategy</option>{branchOptions}
    </select></label>}
    {branch.data && <p>Effective branch strategy · {branchLabels[branch.data.effectiveStrategy]}{branch.data.locked ? " · Locked after provisioning" : ""}</p>}
    {error && <p role="alert">{String(error)}</p>}
    {checklist.data && items.length > 0 && <QueueChecklist scope={scope} mode={checklist.data.effectiveMode} items={items} openQueueJob={openQueueJob} />}
  </section>;
}

function QueueChecklist({ scope, mode, items, openQueueJob }: {
  scope: Scope; mode: ChecklistMode; items: Item[]; openQueueJob?: WorkspaceProps["openQueueJob"];
}) {
  const client = useQueryClient();
  const [itemId, setItemId] = useState("");
  const assignments = useTaskAssignments(scope.projectId, scope.taskId, true);
  const assignment = assignments.data?.find(({ role }) => role === "implementer");
  const target = useQuery({ queryKey: ["project-tasks", scope.projectId, scope.taskId, "checklist-release"],
    queryFn: () => invoke<DispatchTarget | null>("project_checklist_dispatch", scope), refetchInterval: 2500,
  });
  const queue = useMutation({
    mutationFn: ({ releaseId, unitId, selection, item }: { releaseId: string; unitId: string; selection: ModelSelection; item: string | null }) =>
      invoke<Job>("queue_project_checklist_work", { ...scope, expectedReleaseId: releaseId, expectedUnitId: unitId, selection, itemId: item }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["project-job-queue", scope.projectId] }),
    onError: () => target.refetch(),
  });
  const available = mode === "track_progress" || items.some(({ id }) => id === itemId);
  const error = assignments.error ?? target.error ?? queue.error;
  return <section className="pipeline-stack" aria-label="Queue checklist work">
    {mode === "run_by_item" && <label>Checklist item to run <select value={itemId} onChange={(event) => setItemId(event.currentTarget.value)}>
      <option value="">Choose a checklist item</option>{items.map(({ id, body }) => <option key={id} value={id}>{body}</option>)}
    </select></label>}
    {target.data === null && <p>Approve and prepare repository delivery before queuing checklist work.</p>}
    {target.data && !assignment?.modelId && assignments.data && <p>Choose an implementation model in Planned model assignments.</p>}
    <Button type="button" variant="outline" className="justify-self-start" disabled={!available || !target.data || !assignment?.modelId || queue.isPending}
      onClick={() => {
        if (!target.data || !assignment?.modelId) return;
        queue.mutate({ releaseId: target.data.releaseId, unitId: target.data.unitId, item: mode === "run_by_item" ? itemId : null,
          selection: { provider: assignment.provider, profileId: assignment.profileId, modelId: assignment.modelId } });
      }}>{mode === "track_progress" ? "Queue checklist progress" : "Queue selected checklist item"}</Button>
    {queue.data && <p role="status">Checklist work queued. {openQueueJob && <button type="button" className="text-primary underline" onClick={() => openQueueJob(queue.data!.id)}>Open queued job</button>}</p>}
    {error && <p role="alert">{String(error)}</p>}
  </section>;
}
