import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { TaskGoals } from "@/goal-evidence";
import { TaskParticipants } from "@/task-participants";
import { TaskAssignment } from "@/task-assignment";
import { ProjectDescription } from "@/project-description";
import { ProjectAgileTable, ProjectKanban } from "@/project-agile-views";
import { SavedDeliveryMerge, VerifiedDeliveryHistory } from "@/repository-delivery-history";
import {
  canComplete, canMove, checklistQuery, commitQuery, deliveryQuery, epicStateQuery, latestEvidence, questionBlockerLabel, recordedMergeForCommit, statusLabel,
  taskStateQuery, terminalStatus, workflowStates, workDeliveryUnitsQuery, workLevels,
  type AgileRow, type EvidenceTarget, type Hierarchy, type Item, type TaskQuestionBlocker, type Work, type WorkKind,
  type WorkflowState, type WorkflowStatus,
} from "@/project-agile-model";
import type { WorkspaceProps } from "@/workbench-contract";

type Change = (operation: () => Promise<unknown>) => void;
type DeliveryLink = { taskId: string; title: string; deliveryId: string };

function LiveTaskDetails({
  projectId, task, change, busy, links, selectedEvidence, selectedDeliveryId, contributors, openDelivery, openWork, move, check,
}: {
  projectId: string; task: AgileRow; change: Change; busy: boolean; links: WorkspaceProps; contributors: DeliveryLink[];
  selectedEvidence?: EvidenceTarget; selectedDeliveryId?: string; openDelivery: (link: DeliveryLink) => void; openWork: (id: string) => void;
  move: (row: AgileRow, state: WorkflowStatus) => void; check: (id: string, item: Item, checked: boolean) => void;
}) {
  const args = { projectId, taskId: task.id };
  const [itemText, setItemText] = useState("");
  const state = useQuery(taskStateQuery(projectId, task.id));
  const checklist = useQuery(checklistQuery(projectId, task.id));
  const deliveries = useQuery(deliveryQuery(projectId, task.id));
  const units = useQuery(workDeliveryUnitsQuery(projectId, task.id));
  const saved = state.data;
  const question = saved?.questionBlocker;
  const closed = saved && terminalStatus(saved.state);
  const mayCompleteManually = saved && canComplete(saved, task.hasChildren);
  const feasible = workflowStates.filter(({ value }) => canMove({ ...task, workflow: saved }, value));
  const error = state.error ?? checklist.error ?? deliveries.error ?? units.error;
  const recordedUnits = new Set(deliveries.data?.flatMap(({ evidence }) => evidence.kind === "verified_commit" ? evidence.unitEvidenceIds : []));
  const unitHistory = new Map(units.data?.map((entry) => [entry.commit.id, entry]));
  const independentUnits = units.data?.filter(({ commit }) => !recordedUnits.has(commit.id));
  const selectedCommit = selectedEvidence ? unitHistory.get(selectedEvidence.evidenceId)?.commit ??
    (task.commit?.id === selectedEvidence.evidenceId ? task.commit : undefined) : undefined;
  const selectedMerge = selectedCommit && recordedMergeForCommit(selectedCommit, deliveries.data ?? task.deliveries ?? [], saved?.completion);
  const waiting = saved?.waitingForMerge ? "Waiting for merge" : saved?.waitingForCommit ? "Waiting for commit" : undefined;
  return <section className="pipeline-stack" aria-label={task.title}>
    <h3 className="font-medium break-words">{task.title}</h3>
    {saved && <p role="status">{waiting && !question ? `${statusLabel(saved.state)} · ${waiting}` : statusLabel(saved.state)}</p>}
    {feasible.length > 0 && <label className="delivery-field">Move work to <select className="delivery-filter" value="" disabled={busy} onChange={(event) => {
      const next = feasible.find(({ value }) => value === event.currentTarget.value)?.value;
      if (next) move({ ...task, workflow: saved }, next);
    }}><option value="">Choose a state</option>{feasible.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}</select></label>}
    {selectedEvidence && <section className="pipeline-form pipeline-stack" aria-label="Recorded Git evidence">
      <h4>Recorded Git evidence</h4><VerifiedDeliveryHistory {...args} evidenceId={selectedEvidence.evidenceId}
        history={unitHistory.get(selectedEvidence.evidenceId)} openActivity={links.openActivity} openWorkspace={links.openWorkspace} showPatch />
      {selectedMerge && <SavedDeliveryMerge {...args} workflowRunId={selectedMerge.workflowRunId} receipt={selectedMerge} />}
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
    {saved?.pendingMerge && <section className="pipeline-evidence grid gap-1" aria-label="Committed delivery awaiting integration">
      <h4 className="font-medium">Committed delivery awaiting integration</h4>
      <p>Completion SHA <code className="break-all">{saved.pendingMerge.sourceSha}</code></p>
      <p>Working branch <code className="break-all">{saved.pendingMerge.sourceBranch}</code></p>
      <p>Destination branch <code className="break-all">{saved.pendingMerge.destinationBranch}</code></p>
      <Button type="button" variant="outline" onClick={() => links.openWorkspace("pipelines", saved.pendingMerge?.workflowRunId)}>
        Open workflow awaiting merge
      </Button>
    </section>}
    {Boolean(independentUnits?.length) && <section className="pipeline-stack" aria-label="Other committed units">
      <h4 className="font-medium">Other committed units</h4>
      {independentUnits?.map((history) => <details key={history.commit.id} className="pipeline-evidence">
        <summary>{history.commit.purpose}</summary>
        <VerifiedDeliveryHistory {...args} evidenceId={history.commit.id} history={history}
          openActivity={links.openActivity} openWorkspace={links.openWorkspace} />
      </details>)}
    </section>}
    {Boolean(deliveries.data?.length) && <section className="pipeline-stack" aria-label="Delivery history">
      <h4 className="font-medium">{task.kind === "task" ? "Task delivery history" : "Sub-Task delivery history"}</h4>
      {deliveries.data?.map(({ id, evidence, recordedAt, merge }) => <details key={id} className="pipeline-evidence" open={id === selectedDeliveryId || undefined}>
        <summary>{evidence.kind === "manual" ? "Manual completion" : "Verified delivery"} · {new Date(recordedAt).toLocaleString()}</summary>
        {evidence.kind === "manual" ? (evidence.note && <p>{evidence.note}</p>) : <>
          <p>Completion SHA <code className="break-all">{evidence.completionSha}</code></p>
          <SavedDeliveryMerge {...args} workflowRunId={evidence.workflowRunId} receipt={merge} />
          {saved?.activeWorkflowRunId !== evidence.workflowRunId && <Button type="button" variant="outline"
            onClick={() => links.openWorkspace("pipelines", evidence.workflowRunId)}>Open saved workflow</Button>}
          <ul className="pipeline-stack">{evidence.unitEvidenceIds.map((evidenceId) =>
            <li key={evidenceId}><VerifiedDeliveryHistory {...args} evidenceId={evidenceId} history={unitHistory.get(evidenceId)}
              openActivity={links.openActivity} openWorkspace={links.openWorkspace} /></li>)}</ul>
        </>}
      </details>)}
    </section>}
    {contributors.length > 0 && <section className="pipeline-stack" aria-label="Contributing Sub-Task deliveries">
      <h4 className="font-medium">Contributing Sub-Task deliveries</h4>
      <ul className="pipeline-stack">{contributors.map((link) => <li key={link.taskId}>
        <Button type="button" variant="outline" onClick={() => openDelivery(link)}
          aria-label={`Open Sub-Task delivery for ${link.title}`}>{link.title}</Button>
      </li>)}</ul>
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
  const [selectedDelivery, setSelectedDelivery] = useState<DeliveryLink>();
  useEffect(() => { setSelected(props.target); setSelectedEvidence(undefined); setSelectedDelivery(undefined); }, [props.target]);
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
    commit: commits.get(task.id), evidence: evidence[index], deliveries: deliveries[index]?.data, hasChildren: parents.has(task.id),
  }))];
  const current = rows.find(({ id }) => id === selected);
  const contributors = current?.kind === "task" ? rows.flatMap((row) => {
    if (row.kind !== "subtask" || row.parentId !== current.id) return [];
    const delivery = row.workflow?.completion ?? row.deliveries?.at(-1);
    return delivery ? [{ taskId: row.id, title: row.title, deliveryId: delivery.id }] : [];
  }) : [];
  const normalizedSearch = search.trim().toLocaleLowerCase();
  const matched = rows.filter(({ title, status }) => (!filterStatus || status === filterStatus) && title.toLocaleLowerCase().includes(normalizedSearch));
  const kanbanRows = matched.filter(({ kind }) => kind === level);
  const matchIds = new Set(matched.map(({ id }) => id));
  const readError = [...states, ...epicStates, ...deliveries, ...checklists, ...commitReads].find(({ error }) => error)?.error;
  function openWork(id: string) {
    setSelected(id);
    setSelectedEvidence(undefined);
    setSelectedDelivery(undefined);
  }
  const views = {
    selected, busy: mutation.isPending, openWork,
    openEvidence: (target: EvidenceTarget) => { openWork(target.taskId); setSelectedEvidence(target); },
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
          <button type="button" className="justify-self-end text-xs text-muted-foreground hover:underline"
            onClick={() => { setSelected(undefined); setSelectedEvidence(undefined); setSelectedDelivery(undefined); }}>Close detail</button>
          {current.kind === "epic" ? <EpicState key={current.id} projectId={projectId} epic={current} links={props} /> :
            <LiveTaskDetails key={current.id} projectId={projectId} task={current} busy={mutation.isPending}
              change={(operation) => mutation.mutate(operation)} links={props} openWork={openWork} move={move} check={check}
              contributors={contributors} selectedDeliveryId={selectedDelivery?.taskId === current.id ? selectedDelivery.deliveryId : undefined}
              openDelivery={(link) => { openWork(link.taskId); setSelectedDelivery(link); }}
              selectedEvidence={selectedEvidence?.taskId === current.id ? selectedEvidence : undefined} />}
        </div>
      </aside>}
    </div>
  </section>;
}
