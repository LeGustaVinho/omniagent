import { useEffect, useState } from "react";
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Plus } from "lucide-react";
import { Dialog } from "radix-ui";
import type { JobInspection } from "@/bindings/JobInspection";
import { AgileDetailFrame, AgileDetailTabs, AgileSearch, AgileWorkRelations, AgileWorkspaceFrame, type WorkDetailView } from "@/agile-layout";
import { Button } from "@/components/ui/button";
import { TaskGoals } from "@/goal-evidence";
import { TaskParticipants, taskParticipantsQuery } from "@/task-participants";
import { TaskAssignment } from "@/task-assignment";
import { ProjectDescription } from "@/project-description";
import { AgileParticipantFilters, hasParticipantFilter, matchesParticipant, type ParticipantFilter } from "@/project-agile-filters";
import { ProjectAgileTable, ProjectKanban, TaskJobLinks } from "@/project-agile-views";
import { ProjectWorkModes, TaskWorkSettings } from "@/project-agile-settings";
import { ProjectWorkEditor, type ProjectEditorSelection, type WorkspaceWorkInput } from "@/project-work-editor";
import { projectQueueQuery } from "@/queue-model";
import { SavedDeliveryMerge, VerifiedDeliveryHistory } from "@/repository-delivery-history";
import {
  canComplete, canMove, checklistQuery, commitQuery, deliveryQuery, epicStateQuery, latestEvidence, questionBlockerLabel, recordedMergeForCommit, statusLabel,
  taskStateQuery, terminalStatus, workflowStates, workDeliveryUnitsQuery, workLabel, workLevels,
  type AgileRow, type EvidenceTarget, type Hierarchy, type Item, type TaskQuestionBlocker, type Work, type WorkKind,
  type WorkflowState, type WorkflowStatus,
} from "@/project-agile-model";
import type { WorkspaceProps } from "@/workbench-contract";
import { WorkflowLaunchPicker } from "@/workflow-launch";

type Change = (operation: () => Promise<unknown>) => void;
type DeliveryLink = { taskId: string; title: string; deliveryId: string };

function LiveTaskDetails({
  projectId, task, change, busy, links, selectedEvidence, selectedDeliveryId, contributors, openDelivery, openWork, move, check,
  relatedWork, parentWork, childWork, view, selectView, close, edit,
}: {
  projectId: string; task: AgileRow; change: Change; busy: boolean; links: WorkspaceProps; contributors: DeliveryLink[];
  selectedEvidence?: EvidenceTarget; selectedDeliveryId?: string; openDelivery: (link: DeliveryLink) => void; openWork: (id: string) => void;
  move: (row: AgileRow, state: WorkflowStatus) => void; check: (id: string, item: Item, checked: boolean) => void;
  relatedWork: Work[]; parentWork?: AgileRow; childWork: AgileRow[];
  view: WorkDetailView; selectView: (view: WorkDetailView) => void; close: () => void; edit: () => void;
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
  const inspectedEvidence = selectedEvidence ?? task.evidence;
  const selectedCommit = inspectedEvidence ? unitHistory.get(inspectedEvidence.evidenceId)?.commit ??
    (task.commit?.id === inspectedEvidence.evidenceId ? task.commit : undefined) : undefined;
  const selectedMerge = selectedCommit && recordedMergeForCommit(selectedCommit, deliveries.data ?? task.deliveries ?? [], saved?.completion);
  const waiting = saved?.waitingForMerge ? "Waiting for merge" : saved?.waitingForCommit ? "Waiting for commit" : undefined;
  return <AgileDetailFrame label="Work detail" title={task.title} identity={workLabel(task.kind)}
    state={saved ? statusLabel(saved.state) : undefined} note={question ? undefined : waiting}
    closeLabel="Close detail" close={close} navigation={<AgileDetailTabs view={view} onViewChange={selectView} />}>
    <div className="pipeline-stack" hidden={view !== "Overview"}>
      <AgileWorkRelations parent={parentWork && workRelation(parentWork)} children={childWork.map(workRelation)} openWork={openWork} title="Sub-Tasks" />
      {feasible.length > 0 && <label className="delivery-field">Move work to <select className="delivery-filter" value="" disabled={busy} onChange={(event) => {
        const next = feasible.find(({ value }) => value === event.currentTarget.value)?.value;
        if (next) move({ ...task, workflow: saved }, next);
      }}><option value="">Choose a state</option>{feasible.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}</select></label>}
      {question && <section className="pipeline-form pipeline-stack" aria-label="Task question blocker">
        <p role="status">{questionBlockerLabel(question)}</p>
        <Button type="button" variant="outline"
          onClick={() => links.openActivity(question.runId, question.conversationId, question.requestId)}>Open originating question</Button>
      </section>}
      {Boolean(saved?.prerequisiteBlockers?.length) && <section className="pipeline-form pipeline-stack" aria-label="Task dependency blockers">
        <h4>Waiting for dependencies</h4>
        {saved?.prerequisiteBlockers?.map(({ taskId, title }) => <Button key={taskId} type="button" variant="outline" onClick={() => openWork(taskId)}>{title}</Button>)}
      </section>}
      {Boolean(task.blockedJobs?.length) && <section className="pipeline-form pipeline-stack" aria-label="Task job blockers">
        <h4>Job blockers</h4>
        <TaskJobLinks row={task} openQueueJob={links.openQueueJob} />
      </section>}
      <section className="agile-checklist pipeline-form" aria-label="Task checklist">
        <h4 className="font-medium">Checklist</h4>
        {checklist.data?.items.map((item) => <label key={item.id} className="agile-check">
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
        <Button type="button" variant="outline" aria-label={`Edit ${workLabel(task.kind).toLowerCase()}`} disabled={busy} onClick={edit}>Edit {workLabel(task.kind).toLowerCase()}</Button>
        {mayCompleteManually && <Button disabled={busy} onClick={() => change(() =>
          invoke("record_manual_task_delivery", { ...args, note: null }))}>Record manual completion</Button>}
        {closed && (saved?.hasDirectWork || !task.hasChildren) && <Button variant="outline" disabled={busy}
          onClick={() => change(() => invoke("reopen_task_delivery", args))}>Reopen work</Button>}
        {saved && !closed && <Button variant="outline" disabled={busy}
          onClick={() => change(() => invoke("cancel_task_delivery", args))}>Cancel work</Button>}
      </div>
    </div>
    <div className="pipeline-stack" hidden={view !== "Workflow"}>
      {saved?.activeWorkflowRunId && <Button variant="outline" onClick={() => links.openWorkspace("pipelines", saved.activeWorkflowRunId ?? undefined)}>Open delivery workflow</Button>}
      {task.kind !== "epic" && <WorkflowLaunchPicker key={task.id} projectId={projectId}
        checkoutId={links.checkout.checkoutId} workId={task.id} openWorkspace={links.openWorkspace} />}
      {task.kind !== "epic" && <TaskWorkSettings scope={args} workKind={task.kind} candidates={relatedWork}
        items={checklist.data?.items ?? []} openQueueJob={links.openQueueJob} />}
      <TaskAssignment projectId={projectId} taskId={task.id} />
      <TaskGoals projectId={projectId} taskId={task.id} openActivity={links.openActivity} />
    </div>
    <div className="pipeline-stack" hidden={view !== "Context"}>
      <ProjectDescription kind="work" id={task.id} links={links} />
    </div>
    <div className="pipeline-stack" hidden={view !== "Changes"}>
      {inspectedEvidence && view === "Changes" && <section className="pipeline-form pipeline-stack" aria-label="Recorded Git evidence">
        <h4>Recorded Git evidence</h4><VerifiedDeliveryHistory {...args} evidenceId={inspectedEvidence.evidenceId}
          history={unitHistory.get(inspectedEvidence.evidenceId)} openActivity={links.openActivity} openWorkspace={links.openWorkspace} showPatch />
        {selectedMerge && <SavedDeliveryMerge {...args} workflowRunId={selectedMerge.workflowRunId} receipt={selectedMerge} />}
      </section>}
      {saved && !inspectedEvidence && !saved.pendingMerge && <p className="text-sm text-muted-foreground">No verified repository changes recorded.</p>}
      {saved?.pendingMerge && <section className="pipeline-evidence grid gap-1" aria-label="Committed delivery awaiting integration">
        <h4 className="font-medium">Committed delivery awaiting integration</h4>
        <p>Completion SHA <code className="break-all">{saved.pendingMerge.sourceSha}</code></p>
        <p>Working branch <code className="break-all">{saved.pendingMerge.sourceBranch}</code></p>
        <p>Destination branch <code className="break-all">{saved.pendingMerge.destinationBranch}</code></p>
        <Button type="button" variant="outline" onClick={() => links.openWorkspace("pipelines", saved.pendingMerge?.workflowRunId)}>
          Open workflow awaiting merge
        </Button>
      </section>}
    </div>
    <div className="pipeline-stack" hidden={view !== "Work history"}>
      <TaskParticipants projectId={projectId} taskId={task.id} openActivity={links.openActivity} />
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
    </div>
    {error && <p role="alert">{String(error)}</p>}
  </AgileDetailFrame>;
}

function workRelation(work: AgileRow) {
  return { id: work.id, kind: workLabel(work.kind), title: work.title, state: work.status && statusLabel(work.status) };
}

function EpicDetails({ epic, links, tasks, descendants, openWork, openEvidence, close, edit, busy, view, selectView }: {
  epic: AgileRow; links: WorkspaceProps; tasks: AgileRow[]; descendants: AgileRow[];
  openWork: (id: string, view?: WorkDetailView) => void; openEvidence: (evidence: EvidenceTarget) => void;
  close: () => void; edit: () => void; busy: boolean; view: WorkDetailView; selectView: (view: WorkDetailView) => void;
}) {
  const linked = descendants.filter(({ workflow }) => Boolean(workflow?.activeWorkflowRunId));
  const history = descendants.filter(({ attempts, deliveries }) => Boolean(attempts?.length || deliveries?.length));
  const revisions = descendants.filter(({ evidence }) => Boolean(evidence));
  return <AgileDetailFrame label="Work detail" title={epic.title} identity="Epic"
    state={epic.status && statusLabel(epic.status)} closeLabel="Close detail" close={close}
    navigation={<AgileDetailTabs view={view} onViewChange={selectView} />}>
    {view === "Overview" && <>
      <ProjectDescription kind="epic" id={epic.id} links={links} preview />
      <AgileWorkRelations children={tasks.map(workRelation)} openWork={openWork} title="Tasks" />
      {!tasks.length && <p className="text-sm text-muted-foreground">No Tasks linked to this Epic.</p>}
      <Button type="button" variant="outline" aria-label="Edit epic" className="justify-self-start" disabled={busy} onClick={edit}>Edit epic</Button>
    </>}
    {view === "Workflow" && (linked.length > 0 ? <AgileWorkRelations children={linked.map(workRelation)}
      openWork={(id) => openWork(id, "Workflow")} title="Workflows" /> : <p className="text-sm text-muted-foreground">No active workflows in this Epic.</p>)}
    {view === "Work history" && (history.length > 0 ? <AgileWorkRelations children={history.map(workRelation)}
      openWork={(id) => openWork(id, "Work history")} title="Work history" /> : <p className="text-sm text-muted-foreground">No linked attempts or deliveries.</p>)}
    {view === "Context" && <ProjectDescription kind="epic" id={epic.id} links={links} />}
    {view === "Changes" && (revisions.length > 0 ? <AgileWorkRelations children={revisions.map(workRelation)}
      openWork={(id) => { const evidence = revisions.find((row) => row.id === id)?.evidence; if (evidence) openEvidence(evidence); }}
      title="Recorded Git evidence" /> : <p className="text-sm text-muted-foreground">No verified repository changes recorded.</p>)}
  </AgileDetailFrame>;
}

export function ProjectTasksWorkspace(props: WorkspaceProps) {
  const { projectId } = props.checkout;
  const client = useQueryClient();
  const [selected, setSelected] = useState(props.target);
  const [detailView, setDetailView] = useState<WorkDetailView>("Overview");
  const [selectedEvidence, setSelectedEvidence] = useState<EvidenceTarget>();
  const [selectedDelivery, setSelectedDelivery] = useState<DeliveryLink>();
  useEffect(() => {
    setSelected(props.target); setDetailView("Overview"); setSelectedEvidence(undefined); setSelectedDelivery(undefined);
  }, [props.target]);
  const [view, setView] = useState<"kanban" | "table">("kanban");
  const [level, setLevel] = useState<WorkKind>("task");
  const [search, setSearch] = useState("");
  const [filterStatus, setFilterStatus] = useState("");
  const [participantFilter, setParticipantFilter] = useState<ParticipantFilter>({ agent: "", provider: "", model: null });
  const [editing, setEditing] = useState<ProjectEditorSelection>();
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
  const epicStates = useQueries({ queries: epics.map(({ id }) => epicStateQuery(projectId, id)) });
  const states = useQueries({ queries: all.map(({ task }) => taskStateQuery(projectId, task.id)) });
  const checklists = useQueries({ queries: all.map(({ task }) => checklistQuery(projectId, task.id)) });
  const deliveries = useQueries({ queries: all.map(({ task }) => deliveryQuery(projectId, task.id)) });
  const participants = useQueries({ queries: all.map(({ task }) => taskParticipantsQuery(projectId, task.id)) });
  const queue = useQuery({ ...projectQueueQuery(projectId), enabled: Boolean(props.openQueueJob), refetchInterval: 2500 });
  const blockedJobs = new Map<string, JobInspection[]>();
  for (const inspection of queue.data?.jobs ?? []) {
    const { job, blockers } = inspection;
    if (job.projectId !== projectId || !job.taskId || job.queueState === "cancelled" || !blockers.length) continue;
    const jobs = blockedJobs.get(job.taskId);
    if (jobs) jobs.push(inspection);
    else blockedJobs.set(job.taskId, [inspection]);
  }
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
    attempts: participants[index]?.data, blockedJobs: blockedJobs.get(task.id),
  }))];
  const current = rows.find(({ id }) => id === selected);
  const contributors = current?.kind === "task" ? rows.flatMap((row) => {
    if (row.kind !== "subtask" || row.parentId !== current.id) return [];
    const delivery = row.workflow?.completion ?? row.deliveries?.at(-1);
    return delivery ? [{ taskId: row.id, title: row.title, deliveryId: delivery.id }] : [];
  }) : [];
  const normalizedSearch = search.trim().toLocaleLowerCase();
  const byParticipant = hasParticipantFilter(participantFilter);
  const hasFilters = Boolean(search || filterStatus || byParticipant);
  const matched = rows.filter(({ title, status, attempts }) => (!filterStatus || status === filterStatus) &&
    title.toLocaleLowerCase().includes(normalizedSearch) && (!byParticipant || attempts?.some((attempt) => matchesParticipant(attempt, participantFilter))));
  const kanbanRows = matched.filter(({ kind }) => kind === level);
  const matchIds = new Set(matched.map(({ id }) => id));
  const readError = [...states, ...epicStates, ...deliveries, ...checklists, ...commitReads, ...participants, queue].find(({ error }) => error)?.error;
  function openWork(id: string, view: WorkDetailView = "Overview") {
    setSelected(id);
    setDetailView(view);
    setSelectedEvidence(undefined);
    setSelectedDelivery(undefined);
  }
  function closeWork() {
    setSelected(undefined);
    setSelectedEvidence(undefined);
    setSelectedDelivery(undefined);
  }
  const views = {
    selected, busy: mutation.isPending, openWork,
    openHistory: (id: string) => openWork(id, "Work history"),
    openActivity: props.openActivity, openQueueJob: props.openQueueJob,
    openEvidence: (target: EvidenceTarget) => { openWork(target.taskId, "Changes"); setSelectedEvidence(target); },
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
  function save(input: WorkspaceWorkInput) {
    mutation.mutate(async () => {
      if (input.workId) {
        await invoke("update_workspace_work", { projectId, ...input });
      } else {
        const id = await invoke<string>("create_workspace_work", { projectId, ...input });
        openWork(id);
        setLevel(input.placement.kind);
      }
      setEditing(undefined);
    });
  }
  function changeEditorOpen(next: boolean) {
    if (mutation.isPending) return;
    mutation.reset();
    setEditing(next ? { kind: view === "kanban" ? level : "task" } : undefined);
  }
  function edit(work: AgileRow) {
    mutation.reset();
    setEditing({ kind: work.kind, work });
  }
  const related = current ? rows.filter(({ parentId }) => parentId === current.id) : [];
  const relatedIds = new Set(related.map(({ id }) => id));
  const descendants = [...related, ...rows.filter(({ parentId }) => parentId && relatedIds.has(parentId))];
  return <Dialog.Root open={Boolean(editing)} onOpenChange={changeEditorOpen}>
    <AgileWorkspaceFrame title="Tasks" view={view} onViewChange={setView}
      action={<Dialog.Trigger asChild><Button type="button" className="px-3 py-1.5 text-xs aria-disabled:opacity-50" aria-disabled={mutation.isPending}>
        <Plus size={15} aria-hidden="true" /> New work
      </Button></Dialog.Trigger>} filters={<>
      {view === "kanban" && <select aria-label="Kanban level" className="delivery-filter" value={level} onChange={(event) => setLevel(event.currentTarget.value as WorkKind)}>
        {workLevels.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}
      </select>}
      <AgileSearch label="Search work" value={search} onChange={setSearch} />
      <select aria-label="Filter by state" className="delivery-filter" value={filterStatus} onChange={(event) => setFilterStatus(event.currentTarget.value)}>
        <option value="">All states</option>{workflowStates.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}
      </select>
      <AgileParticipantFilters key={projectId} attempts={participants.flatMap(({ data }) => data ?? [])}
        value={participantFilter} onChange={setParticipantFilter} />
      {hasFilters && <button type="button" className="delivery-tab" onClick={() => {
        setSearch(""); setFilterStatus(""); setParticipantFilter({ agent: "", provider: "", model: null });
      }}>Clear filters</button>}
    </>}>
      <div className="delivery-body">
        <div className="delivery-main">
          <ProjectWorkModes projectId={projectId} />
          {(mutation.error || hierarchy.error || readError) && <p className="mx-4" role="alert">{String(mutation.error ?? hierarchy.error ?? readError)}</p>}
          {hierarchy.isPending && <p className="delivery-empty" role="status">Loading project work…</p>}
          {hierarchy.data && !rows.length && <p className="delivery-empty">No project work yet.</p>}
          {byParticipant && participants.some(({ isPending }) => isPending) && <p className="delivery-empty" role="status">Loading work participants…</p>}
          {hierarchy.data && rows.length > 0 && (!byParticipant || !participants.some(({ isPending }) => isPending)) &&
            !(view === "kanban" ? kanbanRows.length : matched.length) && <p className="delivery-empty">No matching work.</p>}
          {view === "kanban" ? <ProjectKanban {...views} rows={kanbanRows} move={move} /> :
            <ProjectAgileTable {...views} rows={rows} matches={matchIds} check={check} />}
        </div>
        {current && (current.kind === "epic" ?
          <EpicDetails key={current.id} epic={current} links={props} tasks={related} descendants={descendants} openWork={openWork}
            openEvidence={views.openEvidence} close={closeWork} edit={() => edit(current)} busy={mutation.isPending} view={detailView} selectView={setDetailView} /> :
          <LiveTaskDetails key={current.id} projectId={projectId} task={current} busy={mutation.isPending} relatedWork={all.map(({ task }) => task)}
            view={detailView} selectView={setDetailView} close={closeWork} edit={() => edit(current)} links={props} openWork={openWork} move={move} check={check}
            parentWork={rows.find(({ id }) => id === current.parentId)} childWork={related}
            change={(operation) => mutation.mutate(operation)}
            contributors={contributors} selectedDeliveryId={selectedDelivery?.taskId === current.id ? selectedDelivery.deliveryId : undefined}
            openDelivery={(link) => { openWork(link.taskId, "Work history"); setSelectedDelivery(link); }}
            selectedEvidence={selectedEvidence?.taskId === current.id ? selectedEvidence : undefined} />)}
      </div>
    </AgileWorkspaceFrame>
    {editing && <ProjectWorkEditor checkout={props.checkout} selection={editing} rows={rows}
      close={() => changeEditorOpen(false)} busy={mutation.isPending} error={mutation.error} save={save} resetError={mutation.reset} />}
  </Dialog.Root>;
}
