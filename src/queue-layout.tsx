import { ListTodo, Pause, Play, Plus, X } from "lucide-react";
import { Fragment, useEffect, useRef, useState, type ComponentProps, type ReactNode } from "react";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import type { RecoveryAction } from "@/bindings/RecoveryAction";
import type { SessionMode } from "@/bindings/SessionMode";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { ContextActions, type MenuGroups } from "@/components/ui/context-actions";
import { queueViews, type JobState, type QueueView } from "@/queue-model";
import { DeliveryDetailPanel, DeliveryStatus } from "@/tasks-git-ui";
import "@/queue-layout.css";

export interface QueueJobRow {
  id: string;
  title: string;
  requester: string;
  status: JobState;
  priority: string | number;
  attempt: string | number;
  reason: string;
  menu?: MenuGroups;
}

interface QueueSelection {
  id: string;
  title: string;
  status: JobState;
  content: ReactNode;
}

export function QueueButton(props: ComponentProps<typeof Button>) {
  return <Button type="button" className="px-2.5 py-1.5 text-xs" {...props} />;
}

export function AddQueueJobButton({ disabled, onClick }: { disabled?: boolean; onClick: () => void }) {
  return <QueueButton disabled={disabled} onClick={onClick}><Plus size={14} aria-hidden /> Add job</QueueButton>;
}

export function QueuePauseButton({ paused, disabled, onClick }: { paused: boolean; disabled?: boolean; onClick: () => void }) {
  return <QueueButton variant="outline" disabled={disabled} onClick={onClick}>
    {paused ? <><Play size={14} aria-hidden /> Resume queue</> : <><Pause size={14} aria-hidden /> Pause queue</>}
  </QueueButton>;
}

export type QueueJobDraft = { selection: ModelSelection; instructions: string; mode: SessionMode };

export function QueueJobForm({ provider, onProvider, options, loading, pending, child, allowWrite, feedback, onSubmit, onCancel }: {
  provider: Provider;
  onProvider: (provider: Provider) => void;
  options: readonly ModelOption[];
  loading?: boolean;
  pending?: boolean;
  child?: boolean;
  allowWrite: boolean;
  feedback?: ReactNode;
  onSubmit: (draft: QueueJobDraft) => void;
  onCancel: () => void;
}) {
  const [selection, setSelection] = useState<ModelSelection | null>(null);
  const [instructions, setInstructions] = useState("");
  const [mode, setMode] = useState<SessionMode>("read_only");
  const selected = options.find(({ selection: model, selectable }) => selectable && model.provider === selection?.provider
    && model.profileId === selection.profileId && model.modelId === selection.modelId);
  const canSubmit = !pending && selected && instructions.trim() && (allowWrite || mode !== "approved_write");

  return <form aria-label={child ? "Managed subagent" : "Managed job"} className="grid gap-3 border-b border-border p-3" onSubmit={(event) => {
    event.preventDefault();
    if (canSubmit) onSubmit({ selection: selected.selection, instructions: instructions.trim(), mode });
  }}>
    <label className="delivery-field">Instructions<textarea required className="delivery-filter min-h-20" value={instructions}
      onChange={(event) => setInstructions(event.currentTarget.value)} disabled={pending} /></label>
    <div className="queue-job-fields">
      <label className="delivery-field">CLI<select className="delivery-filter" value={provider} disabled={pending} onChange={(event) => {
        setSelection(null);
        onProvider(event.currentTarget.value as Provider);
      }}><option value="codex">Codex</option><option value="claude">Claude Code</option><option value="antigravity">Antigravity</option></select></label>
      <ModelPicker label="Model" options={options} loading={loading} disabled={pending}
        selection={{ mode: "single", selected: selection, onChange: setSelection }} />
      <label className="delivery-field">Operation<select className="delivery-filter" value={mode} disabled={pending}
        onChange={(event) => setMode(event.currentTarget.value as SessionMode)}>
        <option value="read_only">Read only</option><option value="plan">Plan</option>
        {allowWrite && <option value="approved_write">Repository write</option>}
      </select></label>
    </div>
    {feedback}
    <QueueJobActions>
      <QueueButton type="submit" disabled={!canSubmit}>{child ? "Queue subagent" : "Queue job"}</QueueButton>
      <QueueButton variant="outline" disabled={pending} onClick={onCancel}>Cancel</QueueButton>
    </QueueJobActions>
  </form>;
}

type JobFact = { label: string; value: ReactNode; mono?: boolean };

export function QueueJobFacts({ facts }: { facts: JobFact[] }) {
  return <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-sm">
    {facts.map(({ label, value, mono }) => <Fragment key={label}>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className={mono ? "break-all font-mono text-xs" : "break-words"}>{value}</dd>
    </Fragment>)}
  </dl>;
}

export function QueueJobActions({ children }: { children: ReactNode }) {
  return <div className="flex flex-wrap gap-2">{children}</div>;
}

export function PendingQueueJobActions({ paused, queuedAnswer, disabled, onPauseToggle, onCancel }: {
  paused: boolean;
  queuedAnswer?: boolean;
  disabled?: boolean;
  onPauseToggle: () => void;
  onCancel: () => void;
}) {
  return <>
    <QueueButton variant="outline" disabled={disabled} onClick={onPauseToggle}>{paused ? "Resume job" : "Pause job"}</QueueButton>
    <QueueButton variant="outline" disabled={disabled} onClick={onCancel}>
      {queuedAnswer ? "Cancel queued answer" : "Cancel pending job"}
    </QueueButton>
  </>;
}

export function QueueRecoveryButton({ action, disabled, onClick }: { action: RecoveryAction; disabled?: boolean; onClick: () => void }) {
  return <QueueButton variant="outline" disabled={disabled} onClick={onClick}>
    {action === "retry" ? "Retry saved input" : "Resume from checkpoint"}
  </QueueButton>;
}

export function QueueLayout({
  occupancy, paused, actions, controls, view, onViewChange, jobs, onSelect, selected, onDeselect, state, onClose,
}: {
  occupancy?: { active: number; capacity: number; description?: string };
  paused: boolean;
  actions: ReactNode;
  controls?: ReactNode;
  view: QueueView;
  onViewChange: (view: QueueView) => void;
  jobs: QueueJobRow[];
  onSelect: (id: string) => void;
  selected?: QueueSelection;
  onDeselect: () => void;
  state?: ReactNode;
  onClose?: () => void;
}) {
  const list = useRef<HTMLDivElement>(null);
  const filters = useRef<HTMLElement>(null);
  const detail = useRef<HTMLHeadingElement>(null);
  const focusSelection = useRef<string>(undefined);
  const selectedId = selected?.id;

  useEffect(() => {
    if (selectedId && focusSelection.current === selectedId) {
      detail.current?.focus();
      focusSelection.current = undefined;
    }
  }, [selectedId]);

  function selectJob(id: string, byKeyboard: boolean) {
    if (byKeyboard && selectedId === id) detail.current?.focus();
    focusSelection.current = byKeyboard && selectedId !== id ? id : undefined;
    onSelect(id);
  }

  function dismissDetails() {
    const row = Array.from(list.current?.querySelectorAll<HTMLButtonElement>("[data-queue-job]") ?? [])
      .find((button) => button.dataset.queueJob === selectedId);
    (row ?? filters.current?.querySelector<HTMLButtonElement>('[aria-pressed="true"]'))?.focus();
    onDeselect();
  }

  return <section className="delivery-view queue-layout" aria-label="Execution queue">
    <header className="delivery-toolbar">
      <div className="queue-heading">
        <span className="queue-heading-icon"><ListTodo size={16} aria-hidden="true" /></span>
        <h2>Queue</h2>
        {occupancy && <span className="queue-occupancy" title={occupancy.description}>
          {occupancy.active} / {occupancy.capacity} slots
        </span>}
        {paused && <DeliveryStatus>Paused</DeliveryStatus>}
      </div>
      <div className="queue-actions">
        {actions}
        {onClose && <Button type="button" variant="ghost" className="queue-icon-button" aria-label="Close queue" onClick={onClose}>
          <X size={17} aria-hidden="true" />
        </Button>}
      </div>
    </header>
    <div className="queue-content">
      <nav ref={filters} className="queue-filters" aria-label="Queue states">
        {queueViews.map((entry) => <button key={entry} type="button" className="delivery-tab" aria-pressed={view === entry}
          onClick={() => onViewChange(entry)}>{entry}</button>)}
      </nav>
      {controls && <div className="queue-controls">{controls}</div>}
      <div className="delivery-body queue-body">
        <div className="delivery-main" ref={list}>
          {state}
          {jobs.length > 0 && <table className="delivery-table queue-job-table" role="table" aria-label="Jobs">
            <thead role="rowgroup"><tr role="row">
              <th role="columnheader" scope="col">Job</th><th role="columnheader" scope="col">Status</th>
              <th role="columnheader" scope="col">Priority</th><th role="columnheader" scope="col">Attempt</th>
              <th role="columnheader" scope="col">Wait reason</th>
            </tr></thead>
            <tbody role="rowgroup">{jobs.map((job) => <tr key={job.id} role="row">
              <td role="cell" className="queue-job-title">
                <ContextActions label={`${job.title} ${job.id}`} groups={job.menu ?? []}>
                  <button type="button" data-queue-job={job.id} className="queue-job-select" aria-pressed={selectedId === job.id}
                    onClick={(event) => selectJob(job.id, event.detail === 0)}>{job.title}</button>
                </ContextActions>
                <div className="queue-job-requester">{job.requester}</div>
              </td>
              <td role="cell" className="queue-job-status"><DeliveryStatus>{job.status}</DeliveryStatus></td>
              <td role="cell" className="queue-job-priority"><span className="queue-compact-label" aria-hidden="true">Priority</span>{job.priority}</td>
              <td role="cell" className="queue-job-attempt"><span className="queue-compact-label" aria-hidden="true">Attempt</span>{job.attempt}</td>
              <td role="cell" className="queue-job-reason"><span className="queue-compact-label" aria-hidden="true">Wait reason</span>{job.reason}</td>
            </tr>)}</tbody>
          </table>}
        </div>
        {selected && <DeliveryDetailPanel className="queue-detail" label="Selected job" resizeKey="queue-job-details">
          <header className="queue-detail-header">
            <div>
              <h3 ref={detail} tabIndex={-1}>{selected.title}</h3>
              <DeliveryStatus>{selected.status}</DeliveryStatus>
            </div>
            <Button type="button" variant="ghost" className="queue-icon-button" aria-label="Close job details" onClick={dismissDetails}>
              <X size={15} aria-hidden="true" />
            </Button>
          </header>
          <div className="delivery-detail-content queue-detail-content">{selected.content}</div>
        </DeliveryDetailPanel>}
      </div>
    </div>
  </section>;
}

export function QueueEmpty({ filtered }: { filtered: boolean }) {
  return <div className="delivery-empty">
    <span className="work-empty-icon"><ListTodo size={20} aria-hidden="true" /></span>
    <h3>{filtered ? "No matching jobs" : "No jobs in this project"}</h3>
    <p>{filtered ? "Choose another status to see more jobs." : "Conversation turns appear here when you send a message."}</p>
  </div>;
}
