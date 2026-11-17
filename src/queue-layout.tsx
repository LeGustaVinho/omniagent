import { ListTodo, X } from "lucide-react";
import { useEffect, useRef, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { queueViews, type JobState, type QueueView } from "@/queue-model";
import { DeliveryStatus } from "@/tasks-git-ui";
import "@/queue-layout.css";

export interface QueueJobRow {
  id: string;
  title: string;
  requester: string;
  status: JobState;
  priority: string | number;
  attempt: string | number;
  reason: string;
}

interface QueueSelection {
  id: string;
  title: string;
  status: JobState;
  content: ReactNode;
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
                <button type="button" data-queue-job={job.id} className="queue-job-select" aria-pressed={selectedId === job.id}
                  onClick={(event) => selectJob(job.id, event.detail === 0)}>{job.title}</button>
                <div className="queue-job-requester">{job.requester}</div>
              </td>
              <td role="cell" className="queue-job-status"><DeliveryStatus>{job.status}</DeliveryStatus></td>
              <td role="cell" className="queue-job-priority"><span className="queue-compact-label" aria-hidden="true">Priority</span>{job.priority}</td>
              <td role="cell" className="queue-job-attempt"><span className="queue-compact-label" aria-hidden="true">Attempt</span>{job.attempt}</td>
              <td role="cell" className="queue-job-reason"><span className="queue-compact-label" aria-hidden="true">Wait reason</span>{job.reason}</td>
            </tr>)}</tbody>
          </table>}
        </div>
        {selected && <aside className="delivery-detail queue-detail" aria-label="Selected job">
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
        </aside>}
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
