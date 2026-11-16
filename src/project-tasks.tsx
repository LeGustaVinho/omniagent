import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { TaskGoals } from "@/goal-evidence";
import { TaskParticipants } from "@/task-participants";
import { TaskAssignment } from "@/task-assignment";
import type { WorkspaceProps } from "@/workbench-contract";

type Work = { id: string; title: string };
type Hierarchy = {
  epics: Work[];
  work: {
    task: Work;
    placement: { kind: "task"; epicId: string | null } | { kind: "subtask"; parentTaskId: string };
  }[];
};
type Item = { id: string; revision: number; position: number; body: string; checked: boolean };
type TaskQuestionBlocker = {
  runId: string;
  conversationId: string;
  requestId: string | number;
  state: "live" | "saving" | "saved" | "needs_review";
  answerRecorded: boolean;
};
type WorkflowState = {
  state: string;
  hasDirectWork: boolean;
  waitingForCommit: boolean;
  checklistResolved: boolean;
  pendingRepositoryChanges: boolean;
  activeWorkflowRunId: string | null;
  questionBlocker: TaskQuestionBlocker | null;
};
type Evidence = { kind: "manual"; note: string | null } |
  { kind: "verified_commit"; completionSha: string; workflowRunId: string; unitEvidenceIds: string[] };
type Delivery = { id: string; recordedAt: string; evidence: Evidence };
type VerifiedCommit = { branch: string; sha: string; captureRunId: string; purpose: string };
type Change = (operation: () => Promise<unknown>) => void;

const stateLabel = (state: string) => state.charAt(0).toUpperCase() + state.slice(1).replaceAll("_", " ");
const questionStatus: Record<TaskQuestionBlocker["state"], string> = {
  live: "Waiting for input",
  saving: "Saving the turn checkpoint…",
  saved: "Waiting for input",
  needs_review: "Question needs review",
};

function CommitDetails({ projectId, taskId, evidenceId, openActivity }: {
  projectId: string; taskId: string; evidenceId: string; openActivity: WorkspaceProps["openActivity"];
}) {
  const commit = useQuery({
    queryKey: ["delivery-verified-commit", projectId, taskId, evidenceId],
    queryFn: () => invoke<VerifiedCommit | null>("verified_delivery_commit", { projectId, taskId, evidenceId }),
  });
  if (commit.error) return <p role="alert">{String(commit.error)}</p>;
  if (!commit.data) return null;
  const evidence = commit.data;
  return <li className="grid gap-1 break-words">
    <span>{evidence.purpose} · {evidence.branch}</span><code className="break-all">{evidence.sha}</code>
    <Button type="button" variant="outline" onClick={() => openActivity(evidence.captureRunId)}>Open implementation attempt</Button>
  </li>;
}

function LiveTaskDetails({ projectId, task, hasChildren, change, busy, links }: {
  projectId: string; task: Work; hasChildren: boolean; change: Change; busy: boolean; links: WorkspaceProps;
}) {
  const args = { projectId, taskId: task.id };
  const [itemText, setItemText] = useState("");
  const state = useQuery({
    queryKey: ["project-tasks", projectId, task.id, "state"],
    queryFn: () => invoke<WorkflowState | null>("project_task_delivery_state", args),
    refetchInterval: ({ state }) => state.data && !["completed", "cancelled"].includes(state.data.state) ? 2000 : false,
  });
  const checklist = useQuery({
    queryKey: ["project-tasks", projectId, task.id, "checklist"],
    queryFn: () => invoke<{ items: Item[] } | null>("project_task_checklist", args),
  });
  const deliveries = useQuery({
    queryKey: ["project-tasks", projectId, task.id, "deliveries"],
    queryFn: () => invoke<Delivery[]>("project_task_deliveries", args),
  });
  const saved = state.data;
  const question = saved?.questionBlocker;
  const closed = saved?.state === "completed" || saved?.state === "cancelled";
  const mayCompleteManually = saved && !closed && !saved.activeWorkflowRunId &&
    !saved.pendingRepositoryChanges && saved.checklistResolved && (!hasChildren || saved.hasDirectWork);
  const error = state.error ?? checklist.error ?? deliveries.error;
  return <section className="pipeline-stack" aria-label={task.title}>
    <h3 className="font-medium break-words">{task.title}</h3>
    {saved && <p role="status">{saved.waitingForCommit && !question ? "In review · Waiting for commit" : stateLabel(saved.state)}</p>}
    {question && <section className="pipeline-form pipeline-stack" aria-label="Task question blocker">
      <p role="status">{question.answerRecorded && question.state !== "needs_review"
        ? "Answer recorded · Waiting to continue" : questionStatus[question.state]}</p>
      <Button type="button" variant="outline"
        onClick={() => links.openActivity(question.runId, question.conversationId, question.requestId)}>Open originating question</Button>
    </section>}
    {saved?.activeWorkflowRunId && <Button variant="outline" onClick={() => links.openWorkspace("pipelines", saved.activeWorkflowRunId ?? undefined)}>Open delivery workflow</Button>}
    <section className="pipeline-form pipeline-stack" aria-label="Task checklist">
      <h4 className="font-medium">Checklist</h4>
      {checklist.data?.items.map((item) => <label key={item.id} className="flex items-center gap-2 break-words">
        <input type="checkbox" checked={item.checked} disabled={busy}
          onChange={(event) => {
            const next = { ...item, checked: event.currentTarget.checked };
            change(() => invoke("revise_project_checklist_item", { ...args, next }));
          }} /> {item.body}
      </label>)}
      <form className="pipeline-actions" onSubmit={(event) => {
        event.preventDefault();
        if (!itemText.trim()) return;
        change(async () => {
          await invoke("append_project_checklist_item", { ...args, body: itemText.trim() });
          setItemText("");
        });
      }}>
        <input aria-label="New checklist item" value={itemText} onChange={(event) => setItemText(event.currentTarget.value)} />
        <Button type="submit" variant="outline" disabled={busy || !itemText.trim()}>Add checklist item</Button>
      </form>
    </section>
    {saved?.pendingRepositoryChanges && <p>Additional repository changes are assigned to this work item.</p>}
    <div className="pipeline-actions">
      {mayCompleteManually && <Button disabled={busy} onClick={() => change(() =>
        invoke("record_manual_task_delivery", { ...args, note: null }))}>Record manual completion</Button>}
      {closed && (saved?.hasDirectWork || !hasChildren) && <Button variant="outline" disabled={busy}
        onClick={() => change(() => invoke("reopen_task_delivery", args))}>Reopen work</Button>}
      {saved && !closed && <Button variant="outline" disabled={busy}
        onClick={() => change(() => invoke("cancel_task_delivery", args))}>Cancel work</Button>}
    </div>
    <TaskAssignment projectId={projectId} taskId={task.id} />
    <TaskGoals projectId={projectId} taskId={task.id} openActivity={links.openActivity} />
    <TaskParticipants projectId={projectId} taskId={task.id} openActivity={links.openActivity} />
    {Boolean(deliveries.data?.length) && <section className="pipeline-stack" aria-label="Delivery history">
      <h4 className="font-medium">Delivery history</h4>
      {deliveries.data?.map(({ id, evidence, recordedAt }) => <details key={id} className="pipeline-evidence">
        <summary>{evidence.kind === "manual" ? "Manual completion" : "Verified delivery"} · {new Date(recordedAt).toLocaleString()}</summary>
        {evidence.kind === "manual" ? (evidence.note && <p>{evidence.note}</p>) : <>
          <p>Completion SHA <code className="break-all">{evidence.completionSha}</code></p>
          {saved?.activeWorkflowRunId !== evidence.workflowRunId && <Button type="button" variant="outline"
            onClick={() => links.openWorkspace("pipelines", evidence.workflowRunId)}>Open saved workflow</Button>}
          <ul className="pipeline-stack">{evidence.unitEvidenceIds.map((evidenceId) =>
            <CommitDetails key={evidenceId} {...args} evidenceId={evidenceId} openActivity={links.openActivity} />)}</ul>
        </>}
      </details>)}
    </section>}
    {error && <p role="alert">{String(error)}</p>}
  </section>;
}

function EpicState({ projectId, epic }: { projectId: string; epic: Work }) {
  const state = useQuery({
    queryKey: ["project-tasks", projectId, epic.id, "state"],
    queryFn: () => invoke<string | null>("project_epic_delivery_state", { projectId, epicId: epic.id }),
  });
  return <section aria-label={epic.title}>
    <h3 className="font-medium">{epic.title}</h3>
    {state.data && <p role="status">{stateLabel(state.data)}</p>}
    {state.error && <p role="alert">{String(state.error)}</p>}
  </section>;
}

export function ProjectTasksWorkspace(props: WorkspaceProps) {
  const { projectId } = props.checkout;
  const client = useQueryClient();
  const [selected, setSelected] = useState(props.target);
  useEffect(() => setSelected(props.target), [props.target]);
  const [kind, setKind] = useState<"epic" | "task" | "subtask">("task");
  const [parent, setParent] = useState("");
  const [title, setTitle] = useState("");
  const hierarchy = useQuery({
    queryKey: ["project-tasks", projectId, "hierarchy"],
    queryFn: () => invoke<Hierarchy>("project_work_hierarchy", { projectId }),
  });
  const mutation = useMutation({
    mutationFn: (operation: () => Promise<unknown>) => operation(),
    onSettled: () => Promise.all([
      client.invalidateQueries({ queryKey: ["project-tasks", projectId] }),
      client.invalidateQueries({ queryKey: ["repository-delivery", projectId] }),
    ]),
  });
  const epics = hierarchy.data?.epics ?? [];
  const all = hierarchy.data?.work ?? [];
  const tasks = all.filter(({ placement }) => placement.kind === "task");
  const children = (taskId: string) => all.filter(({ placement }) => placement.kind === "subtask" && placement.parentTaskId === taskId);
  const selectedId = selected;
  const currentTask = all.find(({ task }) => task.id === selectedId)?.task;
  const currentEpic = epics.find((epic) => epic.id === selectedId);
  const choose = (work: Work) => <button type="button" className="delivery-tab text-left break-words"
    aria-pressed={selectedId === work.id} onClick={() => setSelected(work.id)}>{work.title}</button>;
  const taskRow = ({ task }: Hierarchy["work"][number]) => <li key={task.id}>{choose(task)}
    <ul className="pl-4">{children(task.id).map(({ task: child }) => <li key={child.id}>{choose(child)}</li>)}</ul>
  </li>;
  const add = (event: FormEvent) => {
    event.preventDefault();
    if (!title.trim() || (kind === "subtask" && !parent)) return;
    const placement = kind === "task" ? { kind, epicId: parent || null } :
      kind === "subtask" ? { kind, parentTaskId: parent } : { kind };
    mutation.mutate(async () => {
      const id = await invoke<string>("create_workspace_work", { projectId, title: title.trim(), placement });
      setSelected(id);
      setTitle("");
    });
  };
  return <section className="delivery-view" aria-label="Tasks workspace">
    <header className="delivery-toolbar"><h2>Tasks</h2><Button variant="outline" onClick={() => props.startSimulation("delivery")}>Open task preview</Button></header>
    <div className="delivery-body">
      <div className="delivery-main grid content-start gap-4 p-4">
        <form className="pipeline-form pipeline-stack" onSubmit={add}>
          <label>Work type <select value={kind} onChange={(event) => {
            setKind(event.currentTarget.value as typeof kind);
            setParent("");
          }}><option value="task">Task</option><option value="epic">Epic</option><option value="subtask">Sub-Task</option></select></label>
          {kind !== "epic" && <label>{kind === "task" ? "Epic" : "Parent Task"} <select value={parent} onChange={(event) => setParent(event.currentTarget.value)}>
            <option value="">{kind === "task" ? "Standalone Task" : "Select a Task"}</option>
            {(kind === "task" ? epics : tasks.map(({ task }) => task)).map((item) => <option key={item.id} value={item.id}>{item.title}</option>)}
          </select></label>}
          <label>Title <input value={title} onChange={(event) => setTitle(event.currentTarget.value)} /></label>
          <Button type="submit" disabled={mutation.isPending || !title.trim() || (kind === "subtask" && !parent)}>Create work</Button>
        </form>
        <ul aria-label="Project work">
          {epics.map((epic) => <li key={epic.id}>{choose(epic)}<ul className="pl-4">
            {tasks.filter(({ placement }) => placement.kind === "task" && placement.epicId === epic.id).map(taskRow)}
          </ul></li>)}
          {tasks.filter(({ placement }) => placement.kind === "task" && !placement.epicId).map(taskRow)}
        </ul>
        {hierarchy.data && !epics.length && !all.length && <p className="text-muted-foreground">No project work yet.</p>}
        {currentTask && <LiveTaskDetails key={currentTask.id} projectId={projectId} task={currentTask}
          hasChildren={children(currentTask.id).length > 0} busy={mutation.isPending} change={(operation) => mutation.mutate(operation)} links={props} />}
        {currentEpic && <EpicState projectId={projectId} epic={currentEpic} />}
        {(mutation.error || hierarchy.error) && <p role="alert">{String(mutation.error ?? hierarchy.error)}</p>}
      </div>
    </div>
  </section>;
}
