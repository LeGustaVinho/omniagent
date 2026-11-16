import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { TaskGoals } from "@/goal-evidence";
import { TaskParticipants } from "@/task-participants";
import { TaskAssignment } from "@/task-assignment";
import { ProjectDescription } from "@/project-description";
import { ProjectAgileTable, ProjectKanban } from "@/project-agile-views";
import {
  canComplete, canMove, checklistQuery, commitQuery, deliveryQuery, epicStateQuery, latestEvidence, questionBlockerLabel, statusLabel,
  taskStateQuery, terminalStatus, workflowStates, workLevels,
  type AgileRow, type EvidenceTarget, type Hierarchy, type Item, type TaskQuestionBlocker, type Work, type WorkKind,
  type WorkflowState, type WorkflowStatus,
} from "@/project-agile-model";
import type { WorkspaceProps } from "@/workbench-contract";

type Change = (operation: () => Promise<unknown>) => void;

function CommitDetails({ projectId, taskId, evidenceId, openActivity, openWorkspace, showPatch = false }: {
  projectId: string; taskId: string; evidenceId: string;
  openActivity: WorkspaceProps["openActivity"]; openWorkspace: WorkspaceProps["openWorkspace"]; showPatch?: boolean;
}) {
  const commit = useQuery(commitQuery(projectId, taskId, evidenceId));
  const patch = useQuery({ queryKey: ["delivery-verified-patch", projectId, taskId, evidenceId], enabled: showPatch,
    queryFn: () => invoke<{ patchBase64: string } | null>("verified_delivery_patch", { projectId, taskId, evidenceId }), staleTime: Infinity,
  });
  if (commit.error) return <p role="alert">{String(commit.error)}</p>;
  if (!commit.data) return null;
  const evidence = commit.data;
  return <li className="grid gap-1 break-words">
    <span>{evidence.purpose} · {evidence.branch}</span><code className="break-all">{evidence.sha}</code>
    <Button type="button" variant="outline" onClick={() => openActivity(evidence.captureRunId)}>Open implementation attempt</Button>
    <Button type="button" variant="outline" onClick={() => openWorkspace("git", `attempt:${evidence.captureRunId}`)}>Open saved Git diff</Button>
    {showPatch && <>
      {patch.isPending && <p role="status">Loading accepted diff…</p>}
      {patch.error && <p role="alert">{String(patch.error)}</p>}
      {patch.data === null && <p>Accepted diff unavailable.</p>}
      {patch.data && <pre aria-label="Accepted diff" className="max-h-72 overflow-auto whitespace-pre-wrap rounded bg-muted p-2 text-xs">
        {new TextDecoder().decode(Uint8Array.from(atob(patch.data.patchBase64), (byte) => byte.charCodeAt(0)))}
      </pre>}
    </>}
  </li>;
}

function LiveTaskDetails({ projectId, task, change, busy, links, selectedEvidence, openWork, move, check }: {
  projectId: string; task: AgileRow; change: Change; busy: boolean; links: WorkspaceProps;
  selectedEvidence?: EvidenceTarget; openWork: (id: string) => void;
  move: (row: AgileRow, state: WorkflowStatus) => void; check: (id: string, item: Item, checked: boolean) => void;
}) {
  const args = { projectId, taskId: task.id };
  const [itemText, setItemText] = useState("");
  const state = useQuery(taskStateQuery(projectId, task.id));
  const checklist = useQuery(checklistQuery(projectId, task.id));
  const deliveries = useQuery(deliveryQuery(projectId, task.id));
  const saved = state.data;
  const question = saved?.questionBlocker;
  const closed = saved && terminalStatus(saved.state);
  const mayCompleteManually = saved && canComplete(saved, task.hasChildren);
  const feasible = workflowStates.filter(({ value }) => canMove({ ...task, workflow: saved }, value));
  const error = state.error ?? checklist.error ?? deliveries.error;
  return <section className="pipeline-stack" aria-label={task.title}>
    <h3 className="font-medium break-words">{task.title}</h3>
    {saved && <p role="status">{saved.waitingForCommit && !question ? `${statusLabel(saved.state)} · Waiting for commit` : statusLabel(saved.state)}</p>}
    {feasible.length > 0 && <label className="delivery-field">Move work to <select className="delivery-filter" value="" disabled={busy} onChange={(event) => {
      const next = feasible.find(({ value }) => value === event.currentTarget.value)?.value;
      if (next) move({ ...task, workflow: saved }, next);
    }}><option value="">Choose a state</option>{feasible.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}</select></label>}
    {selectedEvidence && <section className="pipeline-form pipeline-stack" aria-label="Recorded Git evidence">
      <h4>Recorded Git evidence</h4><ul><CommitDetails {...args} evidenceId={selectedEvidence.evidenceId}
        openActivity={links.openActivity} openWorkspace={links.openWorkspace} showPatch /></ul>
    </section>}
    <ProjectDescription kind="work" id={task.id} links={links} />
    {question && <section className="pipeline-form pipeline-stack" aria-label="Task question blocker">
      <p role="status">{questionBlockerLabel(question)}</p>
      <Button type="button" variant="outline"
        onClick={() => links.openActivity(question.runId, question.conversationId, question.requestId)}>Open originating question</Button>
    </section>}
    {Boolean(saved?.prerequisiteBlockers?.length) && <section className="pipeline-form pipeline-stack" aria-label="Task dependency blockers">
      <h4>Waiting for dependencies</h4>
      {saved?.prerequisiteBlockers?.map(({ taskId, title }) => <Button key={taskId} type="button" variant="outline" onClick={() => openWork(taskId)}>{title}</Button>)}
    </section>}
    {saved?.activeWorkflowRunId && <Button variant="outline" onClick={() => links.openWorkspace("pipelines", saved.activeWorkflowRunId ?? undefined)}>Open delivery workflow</Button>}
    <section className="pipeline-form pipeline-stack" aria-label="Task checklist">
      <h4 className="font-medium">Checklist</h4>
      {checklist.data?.items.map((item) => <label key={item.id} className="flex items-center gap-2 break-words">
        <input type="checkbox" checked={item.checked} disabled={busy}
          onChange={(event) => check(task.id, item, event.currentTarget.checked)} /> {item.body}
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
      {closed && (saved?.hasDirectWork || !task.hasChildren) && <Button variant="outline" disabled={busy}
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
            <CommitDetails key={evidenceId} {...args} evidenceId={evidenceId} openActivity={links.openActivity} openWorkspace={links.openWorkspace} />)}</ul>
        </>}
      </details>)}
    </section>}
    {error && <p role="alert">{String(error)}</p>}
  </section>;
}

function EpicState({ projectId, epic, links }: { projectId: string; epic: Work; links: WorkspaceProps }) {
  const state = useQuery(epicStateQuery(projectId, epic.id));
  return <section className="pipeline-stack" aria-label={epic.title}>
    <h3 className="font-medium">{epic.title}</h3>
    {state.data && <p role="status">{statusLabel(state.data)}</p>}
    <ProjectDescription kind="epic" id={epic.id} links={links} />
    {state.error && <p role="alert">{String(state.error)}</p>}
  </section>;
}

export function ProjectTasksWorkspace(props: WorkspaceProps) {
  const { projectId } = props.checkout;
  const client = useQueryClient();
  const [selected, setSelected] = useState(props.target);
  const [selectedEvidence, setSelectedEvidence] = useState<EvidenceTarget>();
  useEffect(() => { setSelected(props.target); setSelectedEvidence(undefined); }, [props.target]);
  const [view, setView] = useState<"kanban" | "table">("kanban");
  const [level, setLevel] = useState<WorkKind>("task");
  const [search, setSearch] = useState("");
  const [filterStatus, setFilterStatus] = useState("");
  const [kind, setKind] = useState<WorkKind>("task");
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
  const epicStates = useQueries({ queries: epics.map(({ id }) => epicStateQuery(projectId, id)) });
  const states = useQueries({ queries: all.map(({ task }) => taskStateQuery(projectId, task.id)) });
  const checklists = useQueries({ queries: all.map(({ task }) => checklistQuery(projectId, task.id)) });
  const deliveries = useQueries({ queries: all.map(({ task }) => deliveryQuery(projectId, task.id)) });
  const evidence = all.map(({ task }, index) => latestEvidence(task.id, deliveries[index]?.data ?? [], states[index]?.data?.completion));
  const commitReads = useQueries({ queries: evidence.flatMap((target) => target ? [commitQuery(projectId, target.taskId, target.evidenceId)] : []) });
  const commits = new Map(evidence.filter((target) => target !== undefined).map((target, index) => [target.taskId, commitReads[index]?.data]));
  const parents = new Set(all.flatMap(({ placement }) => placement.kind === "subtask" ? [placement.parentTaskId] : [placement.epicId]));
  const rows: AgileRow[] = [...epics.map((epic, index): AgileRow => ({
    ...epic, kind: "epic", status: epicStates[index]?.data ?? undefined, checklist: [], hasChildren: parents.has(epic.id),
  })), ...all.map(({ task, placement }, index): AgileRow => ({
    ...task, kind: placement.kind, parentId: placement.kind === "subtask" ? placement.parentTaskId : placement.epicId ?? undefined,
    status: states[index]?.data?.state, workflow: states[index]?.data, checklist: checklists[index]?.data?.items ?? [],
    commit: commits.get(task.id), evidence: evidence[index], hasChildren: parents.has(task.id),
  }))];
  const current = rows.find(({ id }) => id === selected);
  const normalizedSearch = search.trim().toLocaleLowerCase();
  const matched = rows.filter(({ title, status }) => (!filterStatus || status === filterStatus) && title.toLocaleLowerCase().includes(normalizedSearch));
  const kanbanRows = matched.filter(({ kind }) => kind === level);
  const matchIds = new Set(matched.map(({ id }) => id));
  const readError = [...states, ...epicStates, ...deliveries, ...checklists, ...commitReads].find(({ error }) => error)?.error;
  function openWork(id: string) {
    setSelected(id);
    setSelectedEvidence(undefined);
  }
  const views = {
    selected, busy: mutation.isPending, openWork,
    openEvidence: (target: EvidenceTarget) => { setSelected(target.taskId); setSelectedEvidence(target); },
    openQuestion: ({ runId, conversationId, requestId }: TaskQuestionBlocker) => props.openActivity(runId, conversationId, requestId),
  };
  function check(taskId: string, item: Item, checked: boolean) {
    mutation.mutate(() => invoke("revise_project_checklist_item", { projectId, taskId, next: { ...item, checked } }));
  }
  function move(row: AgileRow, nextState: WorkflowStatus) {
    if (!canMove(row, nextState) || mutation.isPending) return;
    mutation.mutate(async () => {
      const state = await invoke<WorkflowState>("transition_project_work", {
        projectId, taskId: row.id, expectedState: row.workflow?.state, nextState,
      });
      client.setQueryData(taskStateQuery(projectId, row.id).queryKey, state);
    });
  }
  const add = (event: FormEvent) => {
    event.preventDefault();
    if (!title.trim() || (kind === "subtask" && !parent)) return;
    const placement = kind === "task" ? { kind, epicId: parent || null } :
      kind === "subtask" ? { kind, parentTaskId: parent } : { kind };
    mutation.mutate(async () => {
      const id = await invoke<string>("create_workspace_work", { projectId, title: title.trim(), placement });
      openWork(id);
      setLevel(kind);
      setTitle("");
    });
  };
  return <section className="delivery-view" aria-label="Tasks workspace">
    <header className="delivery-toolbar flex-wrap">
      <h2>Tasks</h2>
      <nav className="flex items-center gap-1" aria-label="Agile view">
        <button type="button" className="delivery-tab" aria-pressed={view === "kanban"} onClick={() => setView("kanban")}>Kanban</button>
        <button type="button" className="delivery-tab" aria-pressed={view === "table"} onClick={() => setView("table")}>Table</button>
      </nav>
      {view === "kanban" && <select aria-label="Kanban level" className="delivery-filter" value={level} onChange={(event) => setLevel(event.currentTarget.value as WorkKind)}>
        {workLevels.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}
      </select>}
      <input aria-label="Search work" placeholder="Search work" className="delivery-filter" value={search} onChange={(event) => setSearch(event.currentTarget.value)} />
      <select aria-label="Filter by state" className="delivery-filter" value={filterStatus} onChange={(event) => setFilterStatus(event.currentTarget.value)}>
        <option value="">All states</option>{workflowStates.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}
      </select>
    </header>
    <div className="delivery-body">
      <div className="delivery-main">
        <details className="pipeline-form m-4"><summary className="cursor-pointer font-medium">New work</summary>
        <form className="pipeline-stack mt-3" onSubmit={add}>
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
        </details>
        {(mutation.error || hierarchy.error || readError) && <p className="mx-4" role="alert">{String(mutation.error ?? hierarchy.error ?? readError)}</p>}
        {hierarchy.data && !rows.length && <p className="mx-4 text-muted-foreground">No project work yet.</p>}
        {hierarchy.data && rows.length > 0 && !(view === "kanban" ? kanbanRows.length : matched.length) &&
          <p className="mx-4 text-muted-foreground">No matching work.</p>}
        {view === "kanban" ? <ProjectKanban {...views} rows={kanbanRows} move={move} /> :
          <ProjectAgileTable {...views} rows={rows} matches={matchIds} check={check} />}
      </div>
      {current && <aside className="delivery-detail" aria-label="Work detail">
        <div className="delivery-detail-content grid gap-3 p-4">
          <button type="button" className="justify-self-end text-xs text-muted-foreground hover:underline" onClick={() => { setSelected(undefined); setSelectedEvidence(undefined); }}>Close detail</button>
          {current.kind === "epic" ? <EpicState key={current.id} projectId={projectId} epic={current} links={props} /> :
            <LiveTaskDetails key={current.id} projectId={projectId} task={current} busy={mutation.isPending}
              change={(operation) => mutation.mutate(operation)} links={props} openWork={openWork} move={move} check={check}
              selectedEvidence={selectedEvidence?.taskId === current.id ? selectedEvidence : undefined} />}
        </div>
      </aside>}
    </div>
  </section>;
}
