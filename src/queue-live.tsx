import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ArrowRight, ListTodo, RefreshCw, X } from "lucide-react";
import { useState } from "react";
import type { JobInspection } from "@/bindings/JobInspection";
import { Button } from "@/components/ui/button";
import { matchesQueueView, queueBlockerReason, queueJobState, queueViews, type QueueView } from "@/queue-model";
import { DeliveryStatus, InlineIssue } from "@/tasks-git-ui";
import type { WorkspaceProps } from "@/workbench-contract";

interface ProjectQueue {
  settings: { paused: boolean; capacity: number };
  active: number;
  jobs: JobInspection[];
}

function jobTitle({ job }: JobInspection) {
  if (job.context && job.origin.type === "operator") return "Conversation turn";
  return job.origin.type === "agent" ? "Agent job" : "Operator job";
}

function jobRequester({ job }: JobInspection) {
  return job.origin.type === "operator" ? "Operator" : `Agent · ${job.origin.runId}`;
}

function firstWaitReason(inspection: JobInspection) {
  const { job, blockers } = inspection;
  const blocker = blockers[0];
  if (blocker) return queueBlockerReason(blocker);
  if (job.latestAttempt?.cancelRequested) return "Cancellation requested.";
  if (job.latestAttempt) return job.latestAttempt.status === "waiting_for_input" ? "The current attempt needs input." : "—";
  if (job.queueState === "queued") return "Waiting to start.";
  return "—";
}

function JobDetails({ inspection, checkout, onClose, openWorkspace, openActivity, onDependency }: Pick<WorkspaceProps, "checkout" | "openActivity" | "openWorkspace"> & {
  inspection: JobInspection;
  onClose: () => void;
  onDependency: (id: string) => void;
}) {
  const { job, blockers } = inspection;
  const attempt = job.latestAttempt;
  const ownCheckout = job.checkoutId === checkout.checkoutId;
  const conversationId = job.context?.conversationId;
  const requestingRunId = job.origin.type === "agent" ? job.origin.runId : undefined;
  const buttonClass = "px-2.5 py-1.5 text-xs";

  return (
    <aside className="delivery-detail" aria-label="Selected job">
      <div className="delivery-detail-content grid content-start gap-3 p-3.5">
        <div className="flex items-start justify-between gap-2"><h3 className="text-sm font-semibold">{jobTitle(inspection)}</h3><button type="button" aria-label="Close job details" className="rounded p-1 text-muted-foreground hover:bg-muted" onClick={onClose}><X size={15} /></button></div>
        <DeliveryStatus>{queueJobState(inspection)}</DeliveryStatus>
        {attempt?.cancelRequested && <InlineIssue>Cancellation requested. Waiting for the runtime to confirm the outcome.</InlineIssue>}
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-sm">
          <dt className="text-muted-foreground">Job ID</dt><dd className="break-all font-mono text-xs">{job.id}</dd>
          <dt className="text-muted-foreground">Requester</dt><dd className="break-all">{jobRequester(inspection)}</dd>
          <dt className="text-muted-foreground">Worktree</dt><dd className="break-all font-mono text-xs">{ownCheckout ? checkout.checkoutRoot : job.checkoutId}</dd>
          <dt className="text-muted-foreground">Profile mode</dt><dd>{job.mode.replaceAll("_", " ")}</dd>
          <dt className="text-muted-foreground">Priority</dt><dd>{job.priority}</dd>
          <dt className="text-muted-foreground">Attempt</dt><dd>{attempt ? `#${attempt.ordinal}` : "Awaiting dispatch"}</dd>
          {attempt && <><dt className="text-muted-foreground">Run ID</dt><dd className="break-all font-mono text-xs">{attempt.runId}</dd></>}
        </dl>
        {blockers.length > 0 && <section className="grid gap-1.5" aria-label="Admission blockers">
          <h4 className="text-xs font-semibold">Admission blockers</h4>
          <ul className="divide-y divide-border rounded border border-border">
            {blockers.map((blocker, index) => <li key={`${blocker.type}-${index}`} className="grid gap-1.5 p-2 text-xs leading-5">
              <p className="break-words">{queueBlockerReason(blocker)}</p>
              {blocker.type === "dependency" && <button type="button" className="justify-self-start text-primary hover:underline" onClick={() => onDependency(blocker.jobId)}>Inspect dependency</button>}
              {(blocker.type === "conversation_busy" || blocker.type === "writer") && <button type="button" className="justify-self-start text-primary hover:underline" onClick={() => openActivity(blocker.runId, blocker.type === "conversation_busy" ? conversationId : undefined)}>Inspect blocking run</button>}
            </li>)}
          </ul>
        </section>}
        <div className="flex flex-wrap gap-2">
          {conversationId && ownCheckout && <Button className={buttonClass} type="button" variant="outline" onClick={() => openWorkspace("conversations", conversationId)}>Open conversation <ArrowRight size={13} /></Button>}
          {conversationId && !ownCheckout && <p className="w-full text-xs text-muted-foreground">Select the linked worktree to open this conversation.</p>}
          {job.taskId && <Button className={buttonClass} type="button" variant="outline" onClick={() => openWorkspace("tasks", job.taskId ?? undefined)}>Open task <ArrowRight size={13} /></Button>}
          {attempt && <Button className={buttonClass} type="button" variant="outline" onClick={() => openActivity(attempt.runId, conversationId)}>Inspect attempt</Button>}
          {requestingRunId && <Button className={buttonClass} type="button" variant="outline" onClick={() => openActivity(requestingRunId)}>Open requesting run</Button>}
        </div>
      </div>
    </aside>
  );
}

export default function LiveQueue({ checkout, openActivity, openWorkspace, onClose, active = true }: WorkspaceProps & { onClose?: () => void; active?: boolean }) {
  const [view, setView] = useState<QueueView>("All");
  const [selected, setSelected] = useState<string>();
  const query = useQuery({
    queryKey: ["project-job-queue", checkout.projectId],
    queryFn: () => invoke<ProjectQueue>("project_job_queue", { projectId: checkout.projectId }),
    enabled: active,
    refetchInterval: active ? 1000 : false,
    refetchOnWindowFocus: active,
  });
  const visible = query.data?.jobs.filter((job) => matchesQueueView(queueJobState(job), view)) ?? [];
  const selectedJob = query.data?.jobs.find(({ job }) => job.id === selected);

  return (
    <section className="delivery-view" aria-label="Execution queue">
      <header className="delivery-toolbar">
        <ListTodo size={16} className="text-muted-foreground" aria-hidden="true" /><h2>Queue</h2>
        {query.data && <span className="rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground tabular-nums" title="Scheduler occupancy across projects">{query.data.active} / {query.data.settings.capacity} slots</span>}
        {query.data?.settings.paused && <DeliveryStatus>Paused</DeliveryStatus>}
        <div className="ml-auto flex items-center gap-1.5">
          <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" disabled={query.isFetching} onClick={() => void query.refetch()} aria-label="Refresh queue"><RefreshCw size={14} aria-hidden="true" /> Refresh</Button>
          {onClose && <button type="button" aria-label="Close queue" className="rounded p-1 text-muted-foreground hover:bg-muted" onClick={onClose}><X size={17} /></button>}
        </div>
      </header>
      <nav className="flex gap-0.5 overflow-x-auto border-b border-border px-3 py-1" aria-label="Queue states">
        {queueViews.map((entry) => <button type="button" key={entry} className="delivery-tab" aria-pressed={view === entry} onClick={() => setView(entry)}>{entry}</button>)}
      </nav>
      <div className="delivery-body">
        <div className="delivery-main">
          {query.isPending && active && <p className="px-4 py-3 text-sm text-muted-foreground" role="status">Loading project queue…</p>}
          {query.isError && <div className="p-3"><InlineIssue>Could not refresh the project queue. {String(query.error)}</InlineIssue></div>}
          <table className="delivery-table"><thead><tr><th>Job</th><th>Status</th><th>Priority</th><th>Attempt</th><th>Wait reason</th></tr></thead>
            <tbody>{visible.map((inspection) => (
              <tr key={inspection.job.id}>
                <td><button type="button" onClick={() => setSelected(inspection.job.id)} className="text-left font-medium hover:underline" aria-pressed={selected === inspection.job.id}>{jobTitle(inspection)}</button><div className="mt-0.5 break-all text-xs text-muted-foreground">{jobRequester(inspection)}</div></td>
                <td><DeliveryStatus>{queueJobState(inspection)}</DeliveryStatus></td>
                <td className="tabular-nums">{inspection.job.priority}</td>
                <td className="tabular-nums">{inspection.job.latestAttempt?.ordinal ?? "—"}</td>
                <td className="break-words text-muted-foreground">{firstWaitReason(inspection)}</td>
              </tr>
            ))}</tbody>
          </table>
          {query.data && !visible.length && <p className="px-4 py-3 text-sm text-muted-foreground">{query.data.jobs.length ? "No matching jobs." : "No jobs in this project."}</p>}
        </div>
        {selectedJob && <JobDetails inspection={selectedJob} checkout={checkout} onClose={() => setSelected(undefined)}
          openActivity={openActivity} openWorkspace={openWorkspace} onDependency={(id) => { setView("All"); setSelected(id); }} />}
      </div>
    </section>
  );
}
