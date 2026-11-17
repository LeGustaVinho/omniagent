import { DndContext, KeyboardSensor, PointerSensor, useDraggable, useDroppable, useSensor, useSensors, type DragEndEvent } from "@dnd-kit/core";
import { Activity, FileText, GitCompare, GripVertical, Plus } from "lucide-react";
import { Dialog } from "radix-ui";
import { lazy, Suspense, useEffect, useState, type FormEvent } from "react";
import { AgileTable, type WorkEvidence } from "@/agile-table";
import { AgileDetailFrame, AgileDetailTabs, AgileKanbanColumn, AgileSearch, AgileWorkCard, AgileWorkDialog, AgileWorkspaceFrame,
  type WorkDetailView as DetailTab } from "@/agile-layout";
import { ConversationMarkdown } from "@/conversation-markdown";
import { useGitSimulation } from "@/git-simulation";
import { exampleNotes } from "@/ide-knowledge-data";
import { resolveWiki } from "@/knowledge-document";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { ProjectTasksWorkspace } from "@/project-tasks";
import { deliveryConversation } from "@/conversation-preview-data";
import { DeliveryStatus, InlineIssue, IntegrationNotice } from "@/tasks-git-ui";
import {
  deliveryBase, deliveryBlocker, deliveryBranch, deliveryCommit, deliveryModels, deliveryNotePath, deliveryRunId,
  deliveryStageIndex, deliveryStages, deliveryStatus, deliveryTaskId, exampleTasks, modelName,
  releaseTask, taskStates, workCode, workKind, type ChecklistMode, type DeliveryTask, type TaskPriority, type TaskState, type WorkKind, type WorktreeStrategy,
} from "@/tasks-demo-data";
import type { SimulationSession, WorkspaceProps } from "@/workbench-contract";

const levels = ["Task", "Epic", "Sub-Task"] as const;
const levelLabels = { Epic: "Epics", Task: "Tasks", "Sub-Task": "Sub-Tasks" };
const MarkdownEditor = lazy(() => import("@/knowledge-editor").then(({ MarkdownEditor }) => ({ default: MarkdownEditor })));
const descriptionModes = [["source", "Source"], ["live", "Live Preview"], ["reading", "Reading"]] as const;
const priorities = ["Urgent", "High", "Normal", "Low"] as const;
const agents = ["Planning agent", "Cross-review", "Implementation agent", "Verification agent"] as const;

function recordedParticipants(task: DeliveryTask, step?: SimulationSession["delivery"]) {
  if (task.id === deliveryTaskId && step && step !== "note") {
    const max = deliveryStageIndex[step];
    return deliveryStages.filter(({ role }, index) => index <= max && agents.some((agent) => agent === role))
      .map(({ role, model }) => ({ role, model }));
  }
  const fixture = exampleTasks.find(({ id }) => id === task.id);
  if (fixture?.assignment && ["In Progress", "In Review", "Completed", "Failed"].includes(fixture.state)) {
    return [{ role: fixture.state === "In Review" ? "Cross-review" : "Implementation agent", model: modelName(fixture.assignment) }];
  }
  return [];
}

function TaskColumn({ state, tasks, selected, onSelect }: {
  state: TaskState;
  tasks: DeliveryTask[];
  selected?: string;
  onSelect: (id: string) => void;
}) {
  const { setNodeRef, isOver } = useDroppable({ id: state });
  return (
    <AgileKanbanColumn nodeRef={setNodeRef} label={state} count={tasks.length} over={isOver}>
      {tasks.map((task) => <TaskCard key={task.id} task={task} selected={task.id === selected} onSelect={onSelect} />)}
    </AgileKanbanColumn>
  );
}

function TaskCard({ task, selected, onSelect }: { task: DeliveryTask; selected: boolean; onSelect: (id: string) => void }) {
  const canMove = task.id !== deliveryTaskId;
  const { attributes, listeners, setActivatorNodeRef, setNodeRef, transform, isDragging } = useDraggable({ id: task.id, disabled: !canMove });
  const style = transform ? { transform: `translate3d(${transform.x}px, ${transform.y}px, 0)`, zIndex: 2 } : undefined;
  return (
    <AgileWorkCard nodeRef={setNodeRef} identity={workCode(task)} title={task.title} style={style} selected={selected}
      dragging={isDragging} onOpen={() => onSelect(task.id)}
      handle={canMove && <button type="button" className="cursor-grab rounded p-0.5 hover:text-foreground"
        ref={setActivatorNodeRef} {...attributes} {...listeners} aria-label={`Drag ${task.title}`}><GripVertical size={14} aria-hidden="true" /></button>}>
      {task.blocker && <p>{task.blocker}</p>}
      <div className="flex flex-wrap items-center gap-2">
        {workKind(task) !== "Epic" && <><span className="rounded bg-muted px-1.5 py-0.5">{task.priority}</span><span>{modelName(task.assignment)}</span></>}
        {!!task.checklist?.length && <span>{task.checklist.filter(({ checked }) => checked).length}/{task.checklist.length} checked</span>}
      </div>
    </AgileWorkCard>
  );
}

function TaskForm({ task, parents, onSave, hasWorktree }: {
  task: DeliveryTask;
  parents: DeliveryTask[];
  onSave: (task: DeliveryTask) => void;
  hasWorktree: boolean;
}) {
  const [draft, setDraft] = useState(task);
  const [criteria, setCriteria] = useState(task.criteria.join("\n"));
  const [dependencies, setDependencies] = useState(task.dependencies.join("\n"));
  const [descriptionMode, setDescriptionMode] = useState<typeof descriptionModes[number][0]>("live");
  const [linkedNote, setLinkedNote] = useState<string>();
  const kind = workKind(task);
  const canMove = task.id !== deliveryTaskId;
  const validParent = kind !== "Sub-Task" || Boolean(draft.parentId);
  const descriptionPath = workCode(task) + ".md";

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const title = draft.title.trim();
    if (!title || !validParent) return;
    const lines = (value: string) => value.split("\n").map((line) => line.trim()).filter(Boolean);
    onSave({ ...draft, title, criteria: lines(criteria), dependencies: lines(dependencies) });
  }

  return (
    <form className="grid gap-3" onSubmit={submit}>
      <label className="delivery-field">Title<input className="delivery-filter" required value={draft.title} onChange={(event) => setDraft({ ...draft, title: event.target.value })} /></label>
      <section aria-label="Description" className="grid gap-1.5">
        <h4 className="text-sm font-medium">Description</h4>
        <div className="agile-description">
          <div className="flex flex-wrap gap-1 border-b border-border px-1.5 py-1" aria-label="Description views">
            {descriptionModes.map(([mode, label]) => <button key={mode} type="button" className="delivery-tab" aria-pressed={descriptionMode === mode}
              onClick={() => setDescriptionMode(mode)}>{label}</button>)}
          </div>
          {descriptionMode === "reading"
            ? <div className="agile-description-reading">{draft.description
              ? <ConversationMarkdown>{draft.description}</ConversationMarkdown>
              : <p className="text-muted-foreground">No description.</p>}</div>
            : <div className="agile-description-editor"><Suspense fallback={<p className="p-3 text-xs text-muted-foreground" role="status">Loading editor…</p>}>
                <MarkdownEditor mode={descriptionMode} path={descriptionPath} source={draft.description} files={exampleNotes}
                  onChange={(description) => setDraft((current) => ({ ...current, description }))}
                  onLink={(link) => {
                    const locations = resolveWiki(link, descriptionPath, exampleNotes);
                    const [match] = locations;
                    if (match && locations.length === 1) setLinkedNote(match.path);
                  }} />
              </Suspense></div>}
        </div>
        {linkedNote && <details open className="agile-description-reading agile-description">
          <summary className="mb-1 cursor-pointer font-semibold text-primary">{linkedNote}</summary>
          <ConversationMarkdown>{exampleNotes.find(({ path }) => path === linkedNote)?.content ?? ""}</ConversationMarkdown>
        </details>}
      </section>
      {kind !== "Epic" && <label className="delivery-field">{kind === "Task" ? "Epic" : "Parent Task"}
        <select className="delivery-filter" required={kind === "Sub-Task"} value={draft.parentId ?? ""}
          onChange={(event) => setDraft({ ...draft, parentId: event.target.value || undefined })}>
          <option value="">{kind === "Task" ? "No epic" : "Select a task"}</option>
          {parents.map((parent) => <option key={parent.id} value={parent.id}>{workCode(parent)} · {parent.title}</option>)}
        </select>
      </label>}
      <div className="grid grid-cols-2 gap-2">
        {kind !== "Epic" && <label className="delivery-field">Priority<select className="delivery-filter" value={draft.priority} onChange={(event) => setDraft({ ...draft, priority: event.target.value as TaskPriority })}>{priorities.map((priority) => <option key={priority}>{priority}</option>)}</select></label>}
        <label className="delivery-field">State<select className="delivery-filter" disabled={!canMove} value={draft.state} onChange={(event) => setDraft({ ...draft, state: event.target.value as TaskState })}>{taskStates.map((state) => <option key={state}>{state}</option>)}</select></label>
      </div>
      {kind !== "Epic" && <>
      <label className="delivery-field">Checklist execution<select className="delivery-filter" value={draft.checklistMode ?? ""}
        onChange={(event) => setDraft({ ...draft, checklistMode: (event.target.value || undefined) as ChecklistMode | undefined })}>
        <option value="">Inherit default</option><option>Track progress</option><option>Run by item</option>
      </select></label>
      {kind === "Task" && <label className="delivery-field">Branch strategy
        <select className="delivery-filter" disabled={hasWorktree} value={draft.worktreeStrategy ?? ""}
          onChange={(event) => setDraft({ ...draft, worktreeStrategy: (event.target.value || undefined) as WorktreeStrategy | undefined })}>
          <option value="">Inherit project strategy</option>
          <option value="isolated">Branch per work item</option>
          <option value="shared">Shared branch per Task</option>
        </select>
        {hasWorktree && <span className="text-xs text-muted-foreground">This task already has a worktree.</span>}
      </label>}
      <ModelPicker label="Planned assignment" options={deliveryModels} selection={{ mode: "single", selected: draft.assignment, onChange: (assignment) => setDraft({ ...draft, assignment }) }} />
      <label className="delivery-field">Acceptance criteria<textarea className="delivery-filter" rows={3} value={criteria} onChange={(event) => setCriteria(event.target.value)} /></label>
      <label className="delivery-field">Dependencies<textarea className="delivery-filter" rows={2} value={dependencies} onChange={(event) => setDependencies(event.target.value)} /></label>
      {canMove && <label className="delivery-field">Blocker<input className="delivery-filter" value={draft.blocker} onChange={(event) => setDraft({ ...draft, blocker: event.target.value })} /></label>}
      </>}
      <Button type="submit" className="justify-self-start px-2.5 py-1.5 text-xs" disabled={!draft.title.trim() || !validParent}>Save {kind.toLowerCase()}</Button>
    </form>
  );
}

function TaskDialog({ task, parents, onClose, onSave, hasWorktree }: {
  task: DeliveryTask;
  parents: DeliveryTask[];
  onClose: () => void;
  onSave: (task: DeliveryTask) => void;
  hasWorktree: boolean;
}) {
  const kind = workKind(task).toLowerCase();
  const activator = useState(() => document.activeElement)[0];
  return (
    <Dialog.Root open onOpenChange={(next) => { if (!next) onClose(); }}>
      <AgileWorkDialog title={task.title ? `Edit ${kind}` : `New ${kind}`} closeLabel="Close task editor"
        description="Edit the task details, planned assignment, dependencies and criteria."
        onCloseAutoFocus={(event) => {
          if (activator instanceof HTMLElement && activator.getClientRects().length) { event.preventDefault(); activator.focus(); }
        }}>
        <TaskForm task={task} parents={parents} onSave={onSave} hasWorktree={hasWorktree} />
      </AgileWorkDialog>
    </Dialog.Root>
  );
}

function Checklist({ label, entries }: { label: string; entries: string[] }) {
  return (
    <section className="grid gap-1.5">
      <h4 className="text-xs font-semibold">{label}</h4>
      {entries.length > 0
        ? <ul className="list-disc space-y-1 pl-4 text-sm text-muted-foreground">{entries.map((entry, index) => <li key={`${entry}-${index}`}>{entry}</li>)}</ul>
        : <p className="text-sm text-muted-foreground">None</p>}
    </section>
  );
}

function Workflow({ simulation, openWorkspace }: { simulation: SimulationSession; openWorkspace: WorkspaceProps["openWorkspace"] }) {
  const stage = deliveryStageIndex[simulation.delivery];
  return (
    <ol className="grid gap-1">
      {deliveryStages.map(({ name, role, model, info }, index) => {
        const state = index < stage ? "Completed" : index === stage ? (simulation.delivery === "failed" ? "Failed" : simulation.delivery === "interrupted" ? "Interrupted" : simulation.delivery === "question" ? "Waiting for input" : "Ready") : "Backlog";
        return (
          <li key={name} className="grid gap-1.5 border-b border-border py-3">
            <div className="flex flex-wrap items-center justify-between gap-2"><h4 className="text-sm font-medium">{name}</h4><DeliveryStatus>{state}</DeliveryStatus></div>
            <p className="text-xs text-muted-foreground">{role} · {model}</p><p className="text-xs">{info}</p>
            {name === "Approve implementation" && simulation.delivery === "plan" && (
              <Button type="button" className="justify-self-start px-2.5 py-1.5 text-xs" variant="outline" onClick={() => openWorkspace("conversations", deliveryConversation)}>Inspect plan and scope</Button>
            )}
          </li>
        );
      })}
    </ol>
  );
}

function TaskActions({ simulation, openWorkspace, openActivity, openChanges, setTab }: Pick<WorkspaceProps, "openWorkspace" | "openActivity" | "openChanges"> & {
  simulation: SimulationSession;
  setTab: (tab: DetailTab) => void;
}) {
  const git = useGitSimulation();
  const buttonClass = "justify-self-start px-2.5 py-1.5 text-xs";
  switch (simulation.delivery) {
    case "note":
      return <Button className={buttonClass} type="button" onClick={() => openWorkspace("knowledge", deliveryNotePath)}>Open source note</Button>;
    case "question":
      return <Button className={buttonClass} type="button" onClick={() => openWorkspace("conversations", deliveryConversation)}>Answer question</Button>;
    case "plan":
      return <Button className={buttonClass} type="button" onClick={() => setTab("Workflow")}>Review implementation plan</Button>;
    case "queued":
    case "running":
      return <Button className={buttonClass} type="button" onClick={() => openActivity(deliveryRunId)}>Inspect execution</Button>;
    case "review":
      return git.drift
        ? <Button className={buttonClass} type="button" onClick={() => openWorkspace("git", "saved")}>Review destination change</Button>
        : <Button className={buttonClass} type="button" onClick={() => simulation.advanceDelivery("verify")}>Run verification</Button>;
    case "verified":
      return <Button className={buttonClass} type="button" onClick={() => openChanges("saved")}>Review saved diff</Button>;
    case "accepted":
      return <Button className={buttonClass} type="button" onClick={() => openWorkspace("git", "saved")}>Open accepted candidate</Button>;
    case "interrupted":
    case "failed":
      return <Button className={buttonClass} type="button" onClick={() => openActivity(deliveryRunId)}>Inspect recovery options</Button>;
    default:
      return null;
  }
}

function TaskDetails({ task, parent, children, onSelect, onCheck, onAddItem, tab, setTab, close, edit, props, mergeRequired, setMergeRequired }: {
  task: DeliveryTask;
  parent?: DeliveryTask;
  children: DeliveryTask[];
  onSelect: (id: string) => void;
  onCheck: (workId: string, itemId: string, checked: boolean) => void;
  onAddItem: (workId: string, title: string) => void;
  tab: DetailTab;
  setTab: (tab: DetailTab) => void;
  close: () => void;
  edit: () => void;
  props: WorkspaceProps;
  mergeRequired: boolean;
  setMergeRequired: (value: boolean) => void;
}) {
  const sim = props.simulation;
  const isDelivery = task.id === deliveryTaskId && sim !== null;
  const hasCandidate = sim ? ["review", "verified", "accepted", "merged"].includes(sim.delivery) : false;
  const participants = recordedParticipants(task, sim?.delivery);
  const kind = workKind(task);
  const mode = task.checklistMode ?? parent?.checklistMode ?? "Track progress";
  const [itemTitle, setItemTitle] = useState("");
  return (
    <AgileDetailFrame label="Task details" title={task.title} identity={`${kind} · ${workCode(task)}`}
      state={task.state} closeLabel="Close details" close={close} navigation={<AgileDetailTabs view={tab} onViewChange={setTab} />}>
        {tab === "Overview" && <>
          {parent && <button type="button" className="agile-ref justify-self-start" onClick={() => onSelect(parent.id)}>
            {workKind(parent)} · {parent.title}
          </button>}
          {task.description && <div className="text-sm leading-6"><ConversationMarkdown>{task.description}</ConversationMarkdown></div>}
          {task.blocker && <InlineIssue>{task.blocker}</InlineIssue>}
          {kind !== "Epic" && <><dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1.5 text-sm">
            <dt className="text-muted-foreground">Priority</dt><dd>{task.priority}</dd>
            <dt className="text-muted-foreground">Planned assignment</dt><dd>{modelName(task.assignment)}</dd>
            <dt className="text-muted-foreground">Working now</dt><dd>{task.state === "In Progress"
              ? participants.at(-1)?.model ?? "No agent reported"
              : "No active agent"}</dd>
          </dl>
          <Checklist label="Acceptance criteria" entries={task.criteria} />
          <Checklist label="Dependencies" entries={task.dependencies} />
          <section className="agile-checklist" aria-label="Checklist">
            <div className="flex flex-wrap justify-between gap-2">
              <h4 className="text-xs font-semibold">Checklist</h4><span className="text-xs text-muted-foreground">{mode}</span>
            </div>
            <ul>
              {task.checklist?.map((item) => <li key={item.id}><label className="agile-check">
                <input type="checkbox" checked={item.checked} onChange={(event) => onCheck(task.id, item.id, event.currentTarget.checked)} />
                <span>{item.title}</span>
              </label></li>)}
            </ul>
            <form className="flex flex-wrap gap-2" onSubmit={(event) => {
              event.preventDefault();
              if (itemTitle.trim()) { onAddItem(task.id, itemTitle.trim()); setItemTitle(""); }
            }}>
              <input className="delivery-filter flex-1" aria-label="New checklist item" placeholder="Add checklist item" value={itemTitle}
                onChange={(event) => setItemTitle(event.target.value)} />
              <Button variant="outline" type="submit" disabled={!itemTitle.trim()}>Add</Button>
            </form>
            {mode === "Run by item" && <p className="text-xs text-muted-foreground">Manual checks are available. Dispatch by item requires the Agile service.</p>}
          </section></>}
          {!!children.length && <section aria-label={kind === "Epic" ? "Tasks in this epic" : "Sub-Tasks"} className="grid gap-2">
            <h4 className="text-xs font-semibold">{kind === "Epic" ? "Tasks" : "Sub-Tasks"}</h4>
            <ul className="divide-y divide-border">{children.map((child) => <li key={child.id} className="flex items-center justify-between gap-2 py-1.5">
              <button type="button" className="text-left text-sm hover:underline" onClick={() => onSelect(child.id)}>{child.title}</button>
              <DeliveryStatus>{child.state}</DeliveryStatus>
            </li>)}</ul>
          </section>}
          <div className="flex flex-wrap gap-2">
            <Button className="px-2.5 py-1.5 text-xs" type="button" variant="outline" onClick={edit}>Edit {kind.toLowerCase()}</Button>
            {isDelivery && <Button className="px-2.5 py-1.5 text-xs" type="button" variant="outline" onClick={() => props.openWorkspace("pipelines", "task-delivery")}>Run pipeline</Button>}
            {isDelivery && <TaskActions simulation={sim} openWorkspace={props.openWorkspace} openActivity={props.openActivity} openChanges={props.openChanges} setTab={setTab} />}
          </div>
        </>}
        {tab === "Workflow" && (isDelivery ? <Workflow simulation={sim} openWorkspace={props.openWorkspace} /> : <p className="text-sm text-muted-foreground">No workflow linked to this task.</p>)}
        {tab === "Work history" && <>
          <h4 className="text-xs font-semibold">Work history</h4>
          {participants.length ? <ul className="grid divide-y divide-border">{participants.map(({ role, model }) => <li className="flex flex-wrap justify-between gap-2 py-2 text-sm" key={role}><span>{role}</span><span className="text-muted-foreground">{model}</span></li>)}</ul> : <p className="text-sm text-muted-foreground">No linked attempts.</p>}
          {isDelivery && participants.length > 0 && <Button type="button" variant="outline" className="justify-self-start px-2.5 py-1.5 text-xs" onClick={() => props.openActivity(deliveryRunId)}><Activity size={14} /> View run activity</Button>}
        </>}
        {tab === "Context" && (isDelivery
          ? <><h4 className="text-xs font-semibold">Source revision</h4><div className="delivery-card"><p className="break-all font-mono text-xs">{deliveryNotePath}</p><p className="mt-1 text-xs text-muted-foreground">Repository note · {deliveryBase}</p></div><Button type="button" variant="outline" className="justify-self-start px-2.5 py-1.5 text-xs" onClick={() => props.openWorkspace("knowledge", deliveryNotePath)}><FileText size={14} /> Open note</Button></>
          : <p className="text-sm text-muted-foreground">No context linked to this task.</p>)}
        {tab === "Changes" && <>
          {isDelivery && <label className="delivery-field">Completion milestone<select className="delivery-filter" value={mergeRequired ? "merged" : "committed"} onChange={(event) => setMergeRequired(event.target.value === "merged")}><option value="committed">Verified commit</option><option value="merged">Verified merge</option></select></label>}
          {isDelivery && hasCandidate
            ? <><p className="text-sm text-muted-foreground">Saved candidate <code>{deliveryCommit}</code></p><Button type="button" variant="outline" className="justify-self-start px-2.5 py-1.5 text-xs" onClick={() => props.openChanges("saved")}><GitCompare size={14} /> Open saved diff</Button></>
            : <p className="text-sm text-muted-foreground">No candidate diff captured for this task.</p>}
        </>}
    </AgileDetailFrame>
  );
}

export default function TasksWorkspace(props: WorkspaceProps) {
  return props.simulation ? <PreviewTasksWorkspace {...props} /> : <ProjectTasksWorkspace key={props.checkout.projectId} {...props} />;
}

function PreviewTasksWorkspace(props: WorkspaceProps) {
  const { simulation, startSimulation, target } = props;
  const git = useGitSimulation();
  const [tasks, setTasks] = useState<DeliveryTask[]>([releaseTask, ...exampleTasks]);
  const [selected, setSelected] = useState<string>();
  const [tab, setTab] = useState<DetailTab>("Overview");
  const [editing, setEditing] = useState<DeliveryTask>();
  const [view, setView] = useState<"board" | "list">("board");
  const [level, setLevel] = useState<WorkKind>("Task");
  const [query, setQuery] = useState("");
  const [stateFilter, setStateFilter] = useState("");
  const [modelFilter, setModelFilter] = useState("");
  const [providerFilter, setProviderFilter] = useState("");
  const [agentFilter, setAgentFilter] = useState("");
  const { mergeRequired, setMergeRequired } = git;
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 5 } }), useSensor(KeyboardSensor));

  useEffect(() => {
    if (target) { setSelected(target); setTab("Overview"); }
  }, [target]);

  function open(id: string) {
    setSelected(id);
    setTab("Overview");
  }

  function save(task: DeliveryTask) {
    setTasks((current) => current.some(({ id }) => id === task.id)
      ? current.map((existing) => existing.id === task.id ? task : existing)
      : [...current, task]);
    open(task.id);
    setEditing(undefined);
  }

  function move(event: DragEndEvent) {
    const state = event.over?.id as TaskState | undefined;
    if (!state || !taskStates.includes(state) || event.active.id === deliveryTaskId) return;
    setTasks((current) => current.map((task) => task.id === event.active.id ? { ...task, state } : task));
  }

  function check(workId: string, itemId: string, checked: boolean) {
    setTasks((current) => current.map((work) => work.id === workId ? {
      ...work,
      checklist: work.checklist?.map((item) => item.id === itemId ? { ...item, checked } : item),
    } : work));
  }

  function addItem(workId: string, title: string) {
    const item = { id: crypto.randomUUID(), title, checked: false };
    setTasks((current) => current.map((work) => work.id === workId
      ? { ...work, checklist: [...work.checklist ?? [], item] }
      : work));
  }

  function create(kind: WorkKind) {
    const prefix = kind === "Epic" ? "EPIC" : "OPS";
    const existing = new Set(tasks.map(workCode));
    let ordinal = kind === "Epic" ? 2 : 25;
    while (existing.has(`${prefix}-${ordinal}`)) ordinal++;
    setEditing({
      id: `${prefix}-${ordinal}`, kind, title: "", description: "", state: "Backlog", priority: "Normal",
      assignment: null, criteria: [], dependencies: [], blocker: "",
      parentId: kind === "Sub-Task" && selectedTask && workKind(selectedTask) === "Task" ? selectedTask.id : undefined,
    });
  }

  const available = simulation ? tasks.map((task): DeliveryTask => {
    if (task.id !== deliveryTaskId) return task;
    const state = deliveryStatus(simulation.delivery, mergeRequired, git.candidateCommitted);
    const subTasks = tasks.filter(({ parentId }) => parentId === task.id);
    const openChecklist = task.checklist?.some(({ checked }) => !checked);
    const openSubTasks = subTasks.some(({ state }) => state !== "Completed" && state !== "Cancelled");
    if (state === "Completed" && (openChecklist || openSubTasks)) {
      return { ...task, state: "In Review", blocker: openChecklist ? "Finish the checklist to complete this task." : "Waiting for Sub-Tasks to finish." };
    }
    return { ...task, state, blocker: deliveryBlocker(simulation.delivery, mergeRequired, git.candidateCommitted) };
  }) : [];
  const work = available.map((item): DeliveryTask => {
    if (workKind(item) !== "Epic") return item;
    const children = available.filter(({ parentId }) => parentId === item.id);
    if (!children.length || children.some(({ state }) => state !== "Completed" && state !== "Cancelled")) return item;
    return { ...item, state: children.some(({ state }) => state === "Completed") ? "Completed" : "Cancelled" };
  });
  const filter = query.trim().toLocaleLowerCase();
  const visible = work.filter((task) => {
    const participants = recordedParticipants(task, simulation?.delivery);
    const matchingModels = deliveryModels.filter((model) => model.selection.modelId === task.assignment?.modelId
      || participants.some((participant) => participant.model === model.name));
    return (!stateFilter || task.state === stateFilter)
      && (!agentFilter || participants.some(({ role }) => role === agentFilter))
      && (!(modelFilter || providerFilter) || matchingModels.some(({ selection }) =>
        (!modelFilter || selection.modelId === modelFilter) && (!providerFilter || selection.provider === providerFilter)))
      && (!filter || [workCode(task), task.title, task.description, ...task.checklist?.map(({ title }) => title) ?? []].join(" ").toLocaleLowerCase().includes(filter));
  });
  const selectedTask = work.find((task) => task.id === selected);
  const atLevel = visible.filter((task) => workKind(task) === level);
  const columns = stateFilter ? taskStates.filter((state) => state === stateFilter) : taskStates;
  const newKind = view === "board" ? level : "Task";
  function evidence(task: DeliveryTask): WorkEvidence | undefined {
    if (task.id !== deliveryTaskId || !simulation || !["queued", "running", "interrupted", "failed", "review", "verified", "accepted", "merged"].includes(simulation.delivery)) return undefined;
    return { branch: deliveryBranch, commit: git.candidateCommitted ? deliveryCommit : null };
  }

  return (
    <AgileWorkspaceFrame title="Agile Board" view={view === "board" ? "kanban" : "table"}
      onViewChange={(next) => setView(next === "kanban" ? "board" : "list")}
      action={<Button type="button" className="px-3 py-1.5 text-xs" disabled={!simulation} title={simulation ? undefined : "Task creation integration pending"}
          onClick={() => create(newKind)}>
          <Plus size={15} aria-hidden="true" /> New {newKind.toLowerCase()}
        </Button>}
      filters={simulation && <>
        {view === "board" && <select className="delivery-filter font-medium" aria-label="Kanban level" value={level}
          onChange={(event) => setLevel(event.target.value as WorkKind)}>{levels.map((kind) => <option key={kind} value={kind}>{levelLabels[kind]}</option>)}</select>}
        <AgileSearch label="Find a task" value={query} onChange={setQuery} />
        <select className="delivery-filter" aria-label="Filter tasks by state" value={stateFilter} onChange={(event) => setStateFilter(event.target.value)}><option value="">All states</option>{taskStates.map((state) => <option key={state}>{state}</option>)}</select>
        <select className="delivery-filter" aria-label="Filter tasks by provider" value={providerFilter} onChange={(event) => setProviderFilter(event.target.value)}><option value="">All providers</option><option value="codex">Codex</option><option value="claude">Claude Code</option><option value="antigravity">Antigravity</option></select>
        <select className="delivery-filter" aria-label="Filter tasks by agent" disabled={!simulation} value={agentFilter} onChange={(event) => setAgentFilter(event.target.value)}><option value="">All agents</option>{agents.map((agent) => <option key={agent}>{agent}</option>)}</select>
        <ModelPicker label="Filter tasks by model" layout="inline" options={deliveryModels}
          selection={{ mode: "single", selected: deliveryModels.find(({ selection }) => selection.modelId === modelFilter)?.selection ?? null,
            onChange: ({ modelId }) => setModelFilter(modelId), reset: { label: "All models", onSelect: () => setModelFilter("") } }} />
        {(filter || stateFilter || providerFilter || agentFilter || modelFilter) && <button className="delivery-tab" type="button" onClick={() => {
          setQuery(""); setStateFilter(""); setProviderFilter(""); setAgentFilter(""); setModelFilter("");
        }}>Clear filters</button>}
      </>}>
      <div className="delivery-body">
        <div className="delivery-main">
          {!simulation ? <IntegrationNotice title="Tasks are unavailable in this checkout" onSimulate={startSimulation}>Explore the board, task details, dependencies, and reviews with example data.</IntegrationNotice>
            : !(view === "board" ? atLevel.length : visible.length) ? <div className="delivery-empty"><h3>No matching work items</h3><p>Adjust the filters or create a work item.</p></div>
            : view === "board" ? <DndContext sensors={sensors} onDragEnd={move}><div className="delivery-board">{columns.map((state) => <TaskColumn key={state} state={state} tasks={atLevel.filter((task) => task.state === state)} selected={selected} onSelect={open} />)}</div></DndContext>
            : <AgileTable items={work} matches={new Set(visible.map(({ id }) => id))} selected={selected} onSelect={open} onCheck={check}
              evidence={evidence} openGit={props.openChanges} />}
        </div>
        {selectedTask && <TaskDetails key={selectedTask.id} task={selectedTask} parent={work.find(({ id }) => id === selectedTask.parentId)}
          children={work.filter(({ parentId }) => parentId === selectedTask.id)} onSelect={open} onCheck={check} onAddItem={addItem}
          tab={tab} setTab={setTab} close={() => setSelected(undefined)} edit={() => setEditing(selectedTask)} props={props} mergeRequired={mergeRequired} setMergeRequired={setMergeRequired} />}
      </div>
      {editing && <TaskDialog task={editing} parents={work.filter((item) => workKind(item) === (workKind(editing) === "Task" ? "Epic" : "Task"))}
        onClose={() => setEditing(undefined)} onSave={save} hasWorktree={Boolean(evidence(editing))} />}
    </AgileWorkspaceFrame>
  );
}
