import { DndContext, KeyboardSensor, PointerSensor, useDraggable, useDroppable, useSensor, useSensors, type DragEndEvent } from "@dnd-kit/core";
import { Activity, FileText, GitCompare, GripVertical, LayoutGrid, List, Plus, Search, X } from "lucide-react";
import { Dialog } from "radix-ui";
import { useEffect, useState, type FormEvent } from "react";
import { useGitSimulation } from "@/git-simulation";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { deliveryConversation } from "@/conversation-preview-data";
import { DeliveryStatus, InlineIssue, IntegrationNotice } from "@/tasks-git-ui";
import {
  deliveryBase, deliveryBlocker, deliveryCommit, deliveryModels, deliveryNotePath, deliveryRunId,
  deliveryStageIndex, deliveryStages, deliveryStatus, deliveryTaskId, exampleTasks, modelName,
  releaseTask, taskStates, type DeliveryTask, type TaskPriority, type TaskState,
} from "@/tasks-demo-data";
import type { SimulationSession, WorkspaceProps } from "@/workbench-contract";

const tabs = ["Overview", "Workflow", "Runs", "Context", "Changes"] as const;
const priorities = ["Urgent", "High", "Normal", "Low"] as const;
const agents = ["Planning agent", "Cross-review", "Implementation agent", "Verification agent"] as const;
type DetailTab = typeof tabs[number];

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
    <section ref={setNodeRef} className="delivery-column" aria-label={state} data-over={isOver}>
      <header className="flex items-center justify-between px-1 py-1">
        <DeliveryStatus>{state}</DeliveryStatus><span className="text-xs text-muted-foreground tabular-nums">{tasks.length}</span>
      </header>
      {tasks.map((task) => <TaskCard key={task.id} task={task} selected={task.id === selected} onSelect={onSelect} />)}
    </section>
  );
}

function TaskCard({ task, selected, onSelect }: { task: DeliveryTask; selected: boolean; onSelect: (id: string) => void }) {
  const canMove = task.id !== deliveryTaskId;
  const { attributes, listeners, setActivatorNodeRef, setNodeRef, transform, isDragging } = useDraggable({ id: task.id, disabled: !canMove });
  const style = transform ? { transform: `translate3d(${transform.x}px, ${transform.y}px, 0)`, zIndex: 2 } : undefined;
  return (
    <article ref={setNodeRef} style={style} className="delivery-card relative" data-selected={selected} data-dragging={isDragging}>
      <div className="flex items-center justify-between gap-2 text-[11px] text-muted-foreground">
        <span>{task.id === deliveryTaskId ? "OPS-24" : task.id}</span>
        {canMove && <button type="button" className="cursor-grab rounded p-0.5 hover:text-foreground" ref={setActivatorNodeRef} {...attributes} {...listeners} aria-label={`Drag ${task.title}`}><GripVertical size={14} /></button>}
      </div>
      <button type="button" onClick={() => onSelect(task.id)} aria-pressed={selected} className="block w-full py-1.5 text-left text-sm font-medium leading-5 break-words">
        {task.title}
      </button>
      {task.blocker && <p className="mb-2 text-xs leading-5 text-muted-foreground">{task.blocker}</p>}
      <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <span className="rounded bg-muted px-1.5 py-0.5">{task.priority}</span><span>{modelName(task.assignment)}</span>
      </div>
    </article>
  );
}

function TaskForm({ task, onSave, canMove }: { task: DeliveryTask; onSave: (task: DeliveryTask) => void; canMove: boolean }) {
  const [draft, setDraft] = useState(task);
  const [criteria, setCriteria] = useState(task.criteria.join("\n"));
  const [dependencies, setDependencies] = useState(task.dependencies.join("\n"));
  const [subtasks, setSubtasks] = useState(task.subtasks.join("\n"));

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const title = draft.title.trim();
    if (!title) return;
    const lines = (value: string) => value.split("\n").map((line) => line.trim()).filter(Boolean);
    onSave({ ...draft, title, criteria: lines(criteria), dependencies: lines(dependencies), subtasks: lines(subtasks) });
  }

  return (
    <form className="grid gap-3" onSubmit={submit}>
      <label className="delivery-field">Title<input className="delivery-filter" required value={draft.title} onChange={(event) => setDraft({ ...draft, title: event.target.value })} /></label>
      <label className="delivery-field">Description<textarea className="delivery-filter" rows={3} value={draft.description} onChange={(event) => setDraft({ ...draft, description: event.target.value })} /></label>
      <div className="grid grid-cols-2 gap-2">
        <label className="delivery-field">Priority<select className="delivery-filter" value={draft.priority} onChange={(event) => setDraft({ ...draft, priority: event.target.value as TaskPriority })}>{priorities.map((priority) => <option key={priority}>{priority}</option>)}</select></label>
        <label className="delivery-field">State<select className="delivery-filter" disabled={!canMove} value={draft.state} onChange={(event) => setDraft({ ...draft, state: event.target.value as TaskState })}>{taskStates.map((state) => <option key={state}>{state}</option>)}</select></label>
      </div>
      <ModelPicker label="Planned assignment" options={deliveryModels} selection={{ mode: "single", selected: draft.assignment, onChange: (assignment) => setDraft({ ...draft, assignment }) }} />
      <label className="delivery-field">Acceptance criteria<textarea className="delivery-filter" rows={3} value={criteria} onChange={(event) => setCriteria(event.target.value)} /></label>
      <label className="delivery-field">Dependencies<textarea className="delivery-filter" rows={2} value={dependencies} onChange={(event) => setDependencies(event.target.value)} /></label>
      <label className="delivery-field">Subtasks<textarea className="delivery-filter" rows={2} value={subtasks} onChange={(event) => setSubtasks(event.target.value)} /></label>
      {canMove && <label className="delivery-field">Blocker<input className="delivery-filter" value={draft.blocker} onChange={(event) => setDraft({ ...draft, blocker: event.target.value })} /></label>}
      <Button type="submit" className="justify-self-start px-2.5 py-1.5 text-xs" disabled={!draft.title.trim()}>Save task</Button>
    </form>
  );
}

function TaskDialog({ open, task, onClose, onSave }: { open: boolean; task: DeliveryTask; onClose: () => void; onSave: (task: DeliveryTask) => void }) {
  return (
    <Dialog.Root open={open} onOpenChange={(next) => { if (!next) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-black/60" />
        <Dialog.Content className="fixed left-1/2 top-1/2 z-50 max-h-[min(43rem,calc(100dvh-3rem))] w-[min(31rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 overflow-auto rounded-xl border border-border bg-card p-5 shadow-xl">
          <header className="mb-4 flex items-center justify-between gap-3">
            <Dialog.Title className="font-semibold">{task.title ? "Edit task" : "New task"}</Dialog.Title>
            <Dialog.Close aria-label="Close task editor" className="rounded p-1 text-muted-foreground hover:bg-muted"><X size={17} /></Dialog.Close>
          </header>
          <Dialog.Description className="sr-only">Edit the task details, planned assignment, dependencies and criteria.</Dialog.Description>
          {open && <TaskForm task={task} canMove={task.id !== deliveryTaskId} onSave={onSave} />}
        </Dialog.Content>
      </Dialog.Portal>
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

function TaskDetails({ task, tab, setTab, close, edit, props, mergeRequired, setMergeRequired }: {
  task: DeliveryTask;
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
  return (
    <aside className="delivery-detail" aria-label="Task details">
      <header className="flex items-start justify-between gap-3 border-b border-border p-3.5">
        <div className="min-w-0">
          <DeliveryStatus>{task.state}</DeliveryStatus>
          <h3 className="mt-1.5 text-sm font-semibold leading-5 break-words">{task.title}</h3>
        </div>
        <button className="rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground" onClick={close} type="button" aria-label="Close details"><X size={16} /></button>
      </header>
      <nav className="flex gap-0.5 overflow-x-auto border-b border-border px-2 py-1" aria-label="Task detail views">
        {tabs.map((tabName) => <button type="button" key={tabName} className="delivery-tab" aria-pressed={tab === tabName} onClick={() => setTab(tabName)}>{tabName}</button>)}
      </nav>
      <div className="delivery-detail-content grid content-start gap-4 p-3.5">
        {tab === "Overview" && <>
          <p className="text-sm leading-6 text-muted-foreground">{task.description}</p>
          {task.blocker && <InlineIssue>{task.blocker}</InlineIssue>}
          <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1.5 text-sm">
            <dt className="text-muted-foreground">Priority</dt><dd>{task.priority}</dd>
            <dt className="text-muted-foreground">Planned assignment</dt><dd>{modelName(task.assignment)}</dd>
            {isDelivery && <><dt className="text-muted-foreground">Working now</dt><dd>{sim.delivery === "running" ? "GPT-6.1 Sol · Implementation" : "No active agent"}</dd></>}
          </dl>
          <Checklist label="Acceptance criteria" entries={task.criteria} />
          <Checklist label="Dependencies" entries={task.dependencies} />
          <Checklist label="Subtasks" entries={task.subtasks} />
          <div className="flex flex-wrap gap-2">
            <Button className="px-2.5 py-1.5 text-xs" type="button" variant="outline" onClick={edit}>Edit task</Button>
            {isDelivery && <TaskActions simulation={sim} openWorkspace={props.openWorkspace} openActivity={props.openActivity} openChanges={props.openChanges} setTab={setTab} />}
          </div>
        </>}
        {tab === "Workflow" && (isDelivery ? <Workflow simulation={sim} openWorkspace={props.openWorkspace} /> : <p className="text-sm text-muted-foreground">No workflow linked to this task.</p>)}
        {tab === "Runs" && <>
          <h4 className="text-xs font-semibold">Work history</h4>
          {participants.length ? <ul className="grid divide-y divide-border">{participants.map(({ role, model }) => <li className="flex flex-wrap justify-between gap-2 py-2 text-sm" key={role}><span>{role}</span><span className="text-muted-foreground">{model}</span></li>)}</ul> : <p className="text-sm text-muted-foreground">No linked attempts.</p>}
          {isDelivery && participants.length > 0 && <Button type="button" variant="outline" className="justify-self-start px-2.5 py-1.5 text-xs" onClick={() => props.openActivity(deliveryRunId)}><Activity size={14} /> View run activity</Button>}
        </>}
        {tab === "Context" && (isDelivery
          ? <><h4 className="text-xs font-semibold">Source revision</h4><div className="delivery-card"><p className="break-all font-mono text-xs">{deliveryNotePath}</p><p className="mt-1 text-xs text-muted-foreground">Repository note · {deliveryBase}</p></div><Button type="button" variant="outline" className="justify-self-start px-2.5 py-1.5 text-xs" onClick={() => props.openWorkspace("knowledge", deliveryNotePath)}><FileText size={14} /> Open note</Button></>
          : <p className="text-sm text-muted-foreground">No context linked to this task.</p>)}
        {tab === "Changes" && <>
          {isDelivery && <label className="delivery-field">Completion milestone<select className="delivery-filter" value={mergeRequired ? "merged" : "accepted"} onChange={(event) => setMergeRequired(event.target.value === "merged")}><option value="accepted">Diff accepted</option><option value="merged">Merged into destination</option></select></label>}
          {isDelivery && hasCandidate
            ? <><p className="text-sm text-muted-foreground">Saved candidate <code>{deliveryCommit}</code></p><Button type="button" variant="outline" className="justify-self-start px-2.5 py-1.5 text-xs" onClick={() => props.openChanges("saved")}><GitCompare size={14} /> Open saved diff</Button></>
            : <p className="text-sm text-muted-foreground">No candidate diff captured for this task.</p>}
        </>}
      </div>
    </aside>
  );
}

export default function TasksWorkspace(props: WorkspaceProps) {
  const { simulation, startSimulation, target } = props;
  const [tasks, setTasks] = useState<DeliveryTask[]>([releaseTask, ...exampleTasks]);
  const [selected, setSelected] = useState<string>();
  const [tab, setTab] = useState<DetailTab>("Overview");
  const [editing, setEditing] = useState<DeliveryTask>();
  const [view, setView] = useState<"board" | "list">("board");
  const [query, setQuery] = useState("");
  const [stateFilter, setStateFilter] = useState("");
  const [modelFilter, setModelFilter] = useState("");
  const [providerFilter, setProviderFilter] = useState("");
  const [agentFilter, setAgentFilter] = useState("");
  const [mergeRequired, setMergeRequired] = useState(false);
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

  const available = simulation ? tasks.map((task) => task.id === deliveryTaskId
    ? { ...task, state: deliveryStatus(simulation.delivery, mergeRequired), blocker: deliveryBlocker(simulation.delivery, mergeRequired) }
    : task) : [];
  const filter = query.trim().toLocaleLowerCase();
  const visible = available.filter((task) => {
    const participants = recordedParticipants(task, simulation?.delivery);
    const matchingModels = deliveryModels.filter((model) => model.selection.modelId === task.assignment?.modelId
      || participants.some((participant) => participant.model === model.name));
    return (!stateFilter || task.state === stateFilter)
      && (!agentFilter || participants.some(({ role }) => role === agentFilter))
      && (!(modelFilter || providerFilter) || matchingModels.some(({ selection }) =>
        (!modelFilter || selection.modelId === modelFilter) && (!providerFilter || selection.provider === providerFilter)))
      && (!filter || `${task.id} ${task.title} ${task.description}`.toLocaleLowerCase().includes(filter));
  });
  const selectedTask = available.find((task) => task.id === selected);
  const columns = stateFilter ? taskStates.filter((state) => state === stateFilter) : taskStates;

  return (
    <section className="delivery-view" aria-label="Tasks workspace">
      <header className="delivery-toolbar">
        <h2 className="mr-2">Tasks</h2>
        <button type="button" className="delivery-tab" aria-pressed={view === "board"} onClick={() => setView("board")}><LayoutGrid size={14} className="mr-1 inline-block" />Board</button>
        <button type="button" className="delivery-tab" aria-pressed={view === "list"} onClick={() => setView("list")}><List size={14} className="mr-1 inline-block" />List</button>
        <Button type="button" className="ml-auto px-3 py-1.5 text-xs" disabled={!simulation} title={simulation ? undefined : "Task creation integration pending"}
          onClick={() => setEditing({ id: `OPS-${25 + tasks.length - 1 - exampleTasks.length}`, title: "", description: "", state: "Backlog", priority: "Normal", assignment: null, criteria: [], dependencies: [], subtasks: [], blocker: "" })}>
          <Plus size={15} /> New task
        </Button>
      </header>
      <div className="delivery-toolbar min-h-0 py-2">
        <label className="relative min-w-35 max-w-65 flex-1"><Search className="pointer-events-none absolute left-2.5 top-1.5 text-muted-foreground" size={15} /><input className="delivery-filter delivery-search w-full" placeholder="Find a task" aria-label="Find a task" value={query} onChange={(event) => setQuery(event.target.value)} /></label>
        <select className="delivery-filter" aria-label="Filter tasks by state" value={stateFilter} onChange={(event) => setStateFilter(event.target.value)}><option value="">All states</option>{taskStates.map((state) => <option key={state}>{state}</option>)}</select>
        <select className="delivery-filter" aria-label="Filter tasks by provider" value={providerFilter} onChange={(event) => setProviderFilter(event.target.value)}><option value="">All providers</option><option value="codex">Codex</option><option value="claude">Claude Code</option><option value="antigravity">Antigravity</option></select>
        <select className="delivery-filter" aria-label="Filter tasks by agent" disabled={!simulation} value={agentFilter} onChange={(event) => setAgentFilter(event.target.value)}><option value="">All agents</option>{agents.map((agent) => <option key={agent}>{agent}</option>)}</select>
        <select className="delivery-filter" disabled={!simulation} aria-label="Filter tasks by model" value={modelFilter} onChange={(event) => setModelFilter(event.target.value)}><option value="">All models</option>{simulation && deliveryModels.map((model) => <option key={model.selection.modelId} value={model.selection.modelId}>{model.name}</option>)}</select>
      </div>
      <div className="delivery-body">
        <div className="delivery-main">
          {!simulation && <IntegrationNotice title="Task integration pending" onSimulate={startSimulation}>Manage tasks, dependencies and reviews for this project here when the task service is connected.</IntegrationNotice>}
          {view === "board" ? <DndContext sensors={sensors} onDragEnd={move}><div className="delivery-board">{columns.map((state) => <TaskColumn key={state} state={state} tasks={visible.filter((task) => task.state === state)} selected={selected} onSelect={open} />)}</div></DndContext>
            : <table className="delivery-table"><thead><tr><th>Task</th><th>State</th><th>Priority</th><th>Assignment</th></tr></thead><tbody>{visible.map((task) => <tr key={task.id}><td><button type="button" onClick={() => open(task.id)} className="text-left font-medium hover:underline">{task.title}</button></td><td><DeliveryStatus>{task.state}</DeliveryStatus></td><td>{task.priority}</td><td>{modelName(task.assignment)}</td></tr>)}</tbody></table>}
        </div>
        {selectedTask && <TaskDetails task={selectedTask} tab={tab} setTab={setTab} close={() => setSelected(undefined)} edit={() => setEditing(selectedTask)} props={props} mergeRequired={mergeRequired} setMergeRequired={setMergeRequired} />}
      </div>
      {editing && <TaskDialog open task={editing} onClose={() => setEditing(undefined)} onSave={save} />}
    </section>
  );
}
