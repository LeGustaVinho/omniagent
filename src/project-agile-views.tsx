import { DndContext, KeyboardSensor, PointerSensor, useDraggable, useDroppable, useSensor, useSensors, type DragEndEvent } from "@dnd-kit/core";
import { ChevronDown, ChevronRight, GitBranch, GitCommitHorizontal, GripVertical } from "lucide-react";
import { Fragment, useState } from "react";
import { DeliveryStatus } from "@/tasks-git-ui";
import type { VerifiedMerge } from "@/delivery-merge-model";
import { canMove, questionBlockerLabel, recordedMergeForCommit, statusLabel, workLabel, workflowStates, type AgileRow, type EvidenceTarget, type Item, type TaskQuestionBlocker, type WorkflowStatus } from "@/project-agile-model";
import { queueBlockerReason } from "@/queue-model";
import { activeParticipant, participantModel, participantProviderNames, participantStatusNames } from "@/task-participants";
import type { WorkspaceProps } from "@/workbench-contract";

type Views = {
  rows: AgileRow[];
  selected?: string;
  busy: boolean;
  openWork: (id: string) => void;
  openEvidence: (evidence: EvidenceTarget) => void;
  openQuestion: (question: TaskQuestionBlocker) => void;
  openActivity: WorkspaceProps["openActivity"];
  openQueueJob?: WorkspaceProps["openQueueJob"];
};

function ChecklistProgress({ items }: { items: Item[] }) {
  return items.length > 0 ? <span className="tabular-nums">{items.filter(({ checked }) => checked).length}/{items.length} checked</span> : null;
}

export function WorkBlockers({ row, openQuestion, openWork }: Pick<Views, "openQuestion" | "openWork"> & { row: AgileRow }) {
  const work = row.workflow;
  const question = work?.questionBlocker;
  return <>{question && <p>{questionBlockerLabel(question)} · <button type="button" className="text-primary hover:underline" onClick={() => openQuestion(question)}>
    {question.answerRecorded || question.state === "needs_review" ? "Open question" : "Answer question"}
  </button></p>}
    {work?.prerequisiteBlockers?.map((dependency) => <p key={dependency.taskId}>Waiting for <button type="button"
      className="text-primary hover:underline" onClick={() => openWork(dependency.taskId)}>{dependency.title}</button></p>)}
    {work?.waitingForCommit && <p>Waiting for commit</p>}
    {work?.waitingForMerge && <p>Waiting for merge</p>}
    {work?.pendingRepositoryChanges && !work.waitingForCommit && !work.waitingForMerge && <p>Unreviewed repository changes</p>}</>;
}

export function TaskJobLinks({ row, openQueueJob }: Pick<Views, "openQueueJob"> & { row: AgileRow }) {
  if (!openQueueJob) return null;
  return row.blockedJobs?.map(({ job, blockers }) => {
    const approval = blockers.find(({ type }) => type === "approval");
    const blocker = approval ?? blockers[0];
    if (!blocker) return null;
    const action = approval ? "Review job approval" : "Open job blocker";
    return <p key={job.id} className="break-words">{queueBlockerReason(blocker)} <button type="button"
      className="text-primary hover:underline" onClick={() => openQueueJob(job.id)}>{action}</button></p>;
  });
}

function WorkParticipation({ row, openActivity, openWork }: Pick<Views, "openActivity" | "openWork"> & { row: AgileRow }) {
  const working = row.attempts?.filter(({ status }) => activeParticipant(status)) ?? [];
  const past = (row.attempts?.length ?? 0) - working.length;
  return <>
    {working.length > 0 && <section className="grid gap-1" aria-label={`Working now for ${row.title}`}>
      <p className="font-medium">Working now</p>
      {working.map((attempt) => <p key={attempt.runId} className="break-words">
        <button type="button" className="text-left text-primary hover:underline" onClick={() => openActivity(attempt.runId, attempt.conversationId)}
          title={`Open attempt ${attempt.ordinal} · ${attempt.profileId}`}>
          {participantProviderNames[attempt.provider]} · {attempt.profileId} · {participantModel(attempt) ?? "Model unreported"}
        </button> · {participantStatusNames[attempt.status]}
      </p>)}
    </section>}
    {past > 0 && <button type="button" className="justify-self-start text-primary hover:underline"
      aria-label={`Work history for ${row.title}`} onClick={() => openWork(row.id)}>Work history · {past}</button>}
  </>;
}

const evidenceLabels = {
  branch: "Open saved branch", commit: "Open verified commit", destination: "Open destination branch", integration: "Open integration SHA",
};

function EvidenceLink({ row, reference, merge, openEvidence }: {
  row: AgileRow; reference: keyof typeof evidenceLabels; merge?: VerifiedMerge; openEvidence: Views["openEvidence"];
}) {
  const evidence = row.evidence;
  const commit = row.commit;
  if (!evidence || !commit) return null;
  const value = { branch: commit.branch, commit: commit.sha, destination: merge?.destinationBranch, integration: merge?.integrationSha }[reference];
  if (!value) return null;
  const Icon = reference === "branch" || reference === "destination" ? GitBranch : GitCommitHorizontal;
  const description = reference === "destination" ? "Destination" : reference === "integration" ? "Integration" : undefined;
  return <button type="button" className="inline-flex min-w-0 items-start gap-1 text-left font-mono text-xs text-primary hover:underline"
    title={value} onClick={() => openEvidence(evidence)} aria-label={`${evidenceLabels[reference]} ${value}`}>
    <Icon className="shrink-0" size={13} aria-hidden />
    <span className="break-all">
      {description && <small className="block font-sans text-muted-foreground">{description}</small>}
      {reference === "commit" ? value.slice(0, 12) : value}
    </span>
  </button>;
}

function KanbanCard({ row, selected, busy, openWork, openEvidence, openQuestion, openActivity, openQueueJob }: Omit<Views, "rows"> & { row: AgileRow }) {
  const allowed = !busy && workflowStates.some(({ value }) => canMove(row, value));
  const { attributes, listeners, setActivatorNodeRef, setNodeRef, transform, isDragging } = useDraggable({ id: row.id, disabled: !allowed });
  return <article ref={setNodeRef} className="delivery-card relative" data-selected={selected === row.id} data-dragging={isDragging}
    style={transform ? { transform: `translate3d(${transform.x}px, ${transform.y}px, 0)`, zIndex: 2 } : undefined}>
    <div className="flex items-start justify-between gap-2">
      <button type="button" onClick={() => openWork(row.id)} aria-pressed={selected === row.id} className="min-w-0 flex-1 break-words text-left text-sm font-medium hover:underline">{row.title}</button>
      {allowed && <button type="button" ref={setActivatorNodeRef} {...attributes} {...listeners} aria-label={`Drag ${row.title}`}
        className="shrink-0 cursor-grab rounded p-0.5 text-muted-foreground hover:text-foreground"><GripVertical size={16} aria-hidden /></button>}
    </div>
    <div className="mt-1.5 grid gap-1 text-xs text-muted-foreground">
      <ChecklistProgress items={row.checklist} />
      <WorkBlockers row={row} openWork={openWork} openQuestion={openQuestion} />
      <TaskJobLinks row={row} openQueueJob={openQueueJob} />
      <WorkParticipation row={row} openWork={openWork} openActivity={openActivity} />
      {row.commit && <div className="flex flex-wrap gap-x-2 gap-y-1">
        <EvidenceLink row={row} reference="branch" openEvidence={openEvidence} />
        <EvidenceLink row={row} reference="commit" openEvidence={openEvidence} />
      </div>}
    </div>
  </article>;
}

function KanbanColumn({ status, label, rows, ...views }: Views & { status: WorkflowStatus; label: string }) {
  const { setNodeRef, isOver } = useDroppable({ id: status });
  return <section ref={setNodeRef} className="delivery-column" aria-label={label} data-over={isOver}>
    <header className="flex items-center justify-between px-1 py-1">
      <DeliveryStatus>{label}</DeliveryStatus><span className="text-xs tabular-nums text-muted-foreground">{rows.length}</span>
    </header>
    {rows.map((row) => <KanbanCard key={row.id} row={row} {...views} />)}
  </section>;
}

export function ProjectKanban({ move, rows, ...views }: Views & { move: (row: AgileRow, next: WorkflowStatus) => void }) {
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 5 } }), useSensor(KeyboardSensor));
  function drop({ active, over }: DragEndEvent) {
    const row = rows.find(({ id }) => id === active.id);
    const next = workflowStates.find(({ value }) => value === over?.id)?.value;
    if (row && next && canMove(row, next) && !views.busy) move(row, next);
  }
  return <DndContext sensors={sensors} onDragEnd={drop}>
    <div className="delivery-board" aria-label="Agile Kanban">
      {workflowStates.map(({ value, label }) => <KanbanColumn key={value} status={value} label={label}
        rows={rows.filter(({ status }) => status === value)} {...views} />)}
    </div>
  </DndContext>;
}

export function ProjectAgileTable({ rows, matches, selected, busy, openWork, openEvidence, openQuestion, openActivity, openQueueJob, check }: Views & {
  matches: ReadonlySet<string>;
  check: (taskId: string, item: Item, checked: boolean) => void;
}) {
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const children = new Map<string | undefined, AgileRow[]>();
  for (const row of rows) children.set(row.parentId, [...(children.get(row.parentId) ?? []), row]);
  const containsMatch = (row: AgileRow): boolean => matches.has(row.id) || Boolean(children.get(row.id)?.some(containsMatch));
  function toggle(id: string) {
    setExpanded((prior) => {
      const next = new Set(prior);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }
  function renderRows(entries: AgileRow[], depth: number) {
    return entries.filter(containsMatch).map((row) => {
      const nested = children.get(row.id)?.filter(containsMatch) ?? [];
      const items = matches.has(row.id) ? row.checklist : [];
      const open = expanded.has(row.id);
      const merge = row.commit && row.evidence?.evidenceId === row.commit.id
        ? recordedMergeForCommit(row.commit, row.deliveries ?? [], row.workflow?.completion) : undefined;
      return <Fragment key={row.id}>
        <tr data-selected={row.id === selected}>
          <td style={{ paddingInlineStart: `${0.7 + depth * 1.25}rem` }}>
            <div className="agile-work-cell">
              {nested.length > 0 || items.length > 0 ? <button type="button" className="agile-disclosure" aria-expanded={open}
                aria-label={`${open ? "Collapse" : "Expand"} ${row.title}`} onClick={() => toggle(row.id)}>
                {open ? <ChevronDown size={15} aria-hidden /> : <ChevronRight size={15} aria-hidden />}
              </button> : <span className="agile-disclosure" aria-hidden />}
              <button type="button" className="agile-work-name" aria-pressed={row.id === selected} onClick={() => openWork(row.id)}>
                <strong>{row.title}</strong><small>{workLabel(row.kind)}</small>
              </button>
            </div>
          </td>
          <td>{row.status ? <DeliveryStatus>{statusLabel(row.status)}</DeliveryStatus> : <span>Loading state…</span>}
            <div className="mt-1 grid gap-1 text-xs text-muted-foreground">
              <WorkBlockers row={row} openWork={openWork} openQuestion={openQuestion} />
              <TaskJobLinks row={row} openQueueJob={openQueueJob} />
              <WorkParticipation row={row} openWork={openWork} openActivity={openActivity} />
            </div></td>
          <td><ChecklistProgress items={items} /></td>
          <td><div className="grid gap-1">
            <EvidenceLink row={row} reference="branch" openEvidence={openEvidence} />
            <EvidenceLink row={row} reference="destination" merge={merge} openEvidence={openEvidence} />
          </div></td>
          <td><div className="grid gap-1">
            <EvidenceLink row={row} reference="commit" openEvidence={openEvidence} />
            <EvidenceLink row={row} reference="integration" merge={merge} openEvidence={openEvidence} />
          </div></td>
        </tr>
        {open && items.map((item) => <tr key={item.id} className="agile-checklist-row">
          <td style={{ paddingInlineStart: `${1 + (depth + 1) * 1.25}rem` }}>
            <label className="agile-check"><input type="checkbox" checked={item.checked} disabled={busy}
              onChange={(event) => check(row.id, item, event.currentTarget.checked)} />{item.body}</label>
          </td>
          <td>{item.checked ? "Checked" : "Open"}</td><td colSpan={3}>Checklist item</td>
        </tr>)}
        {open && renderRows(nested, depth + 1)}
      </Fragment>;
    });
  }
  return <div className="overflow-auto"><table className="delivery-table agile-table" aria-label="Agile table">
    <thead><tr><th>Work</th><th>State</th><th>Checklist</th><th>Branch</th><th>Commit</th></tr></thead>
    <tbody>{renderRows(children.get(undefined) ?? [], 0)}</tbody>
  </table></div>;
}
