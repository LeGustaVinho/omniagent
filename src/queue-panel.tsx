import { AlertTriangle, ArrowRight, CircleCheck, CircleStop, Clock3, Pause, Play, Plus, RotateCcw, X } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { deliveryConversation } from "@/conversation-preview-data";
import { ModelPicker } from "@/components/model-picker";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { deliveryBranch, deliveryModels, deliveryRunId, deliveryTaskId, modelName } from "@/tasks-demo-data";
import LiveQueue from "@/queue-live";
import { QueueEmpty, QueueLayout } from "@/queue-layout";
import { matchesQueueView, type JobState, type QueueView } from "@/queue-model";
import { InlineIssue } from "@/tasks-git-ui";
import type { DeliveryAction, DeliveryStep, QueueJobFocus, WorkspaceProps } from "@/workbench-contract";

interface Job {
  id: string;
  title: string;
  requester: string;
  state: JobState;
  priority: string;
  reason: string;
  attempt: number;
  branch: string;
  assignment?: ModelSelection | null;
}

const auxiliaryJobs: Job[] = [
  { id: "docs", title: "Index repository notes", requester: "Knowledge", state: "Queued", priority: "Normal", reason: "Waiting for the execution slot", attempt: 1, branch: "main", assignment: deliveryModels[2]?.selection },
  { id: "bootstrap", title: "Check direct runner", requester: "Diagnostics", state: "Completed", priority: "Normal", reason: "Runner replied successfully", attempt: 1, branch: "main", assignment: deliveryModels[0]?.selection },
];

const releaseQueue: Record<DeliveryStep, Pick<Job, "state" | "reason">> = {
  note: { state: "Queued", reason: "Waiting for a source note" },
  question: { state: "Waiting for input", reason: "The planner needs a target platform selection" },
  plan: { state: "Waiting for input", reason: "Implementation approval is required" },
  queued: { state: "Queued", reason: "Writer slot is available" },
  running: { state: "Running", reason: "The implementation agent holds the worktree reservation" },
  review: { state: "Completed", reason: "Candidate captured for verification" },
  verified: { state: "Completed", reason: "All required checks passed" },
  accepted: { state: "Completed", reason: "Diff accepted" },
  merged: { state: "Completed", reason: "Destination contains the accepted candidate" },
  interrupted: { state: "Interrupted", reason: "The writer disconnected after its checkpoint was recorded" },
  failed: { state: "Failed", reason: "The release smoke check exited with code 1" },
};

const actions: Partial<Record<DeliveryStep, { action: DeliveryAction; title: string }>> = {
  queued: { action: "dispatch", title: "Start attempt" },
  running: { action: "finish", title: "Capture candidate" },
  interrupted: { action: "resume", title: "Resume from checkpoint" },
  failed: { action: "retry", title: "Retry as linked attempt" },
};

function QueueJobForm({ onAdd, onCancel }: { onAdd: (title: string, assignment: ModelSelection) => void; onCancel: () => void }) {
  const [title, setTitle] = useState("");
  const [assignment, setAssignment] = useState<ModelSelection | null>(null);

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (title.trim() && assignment) onAdd(title.trim(), assignment);
  }

  return (
    <form className="grid gap-3 border-b border-border p-3 sm:grid-cols-[minmax(10rem,1fr)_minmax(12rem,1fr)_auto]" onSubmit={submit}>
      <label className="delivery-field self-start">Job name<input required className="delivery-filter" value={title} onChange={(event) => setTitle(event.target.value)} /></label>
      <ModelPicker label="Model" options={deliveryModels} selection={{ mode: "single", selected: assignment, onChange: setAssignment }} />
      <div className="flex items-start gap-2">
        <Button type="submit" className="px-2.5 py-1.5 text-xs" disabled={!title.trim() || !assignment}>Add to queue</Button>
        <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" onClick={onCancel}>Cancel</Button>
      </div>
    </form>
  );
}

export default function QueuePanel(props: WorkspaceProps & { onClose?: () => void; active?: boolean; focusJob?: QueueJobFocus }) {
  return props.simulation ? <SimulatedQueue {...props} /> : <LiveQueue {...props} />;
}

function SimulatedQueue({ simulation, openWorkspace, openActivity, onClose }: WorkspaceProps & { onClose?: () => void }) {
  const [paused, setPaused] = useState(false);
  const [view, setView] = useState<QueueView>("All");
  const [jobs, setJobs] = useState(auxiliaryJobs);
  const [selected, setSelected] = useState<string>(deliveryTaskId);
  const [adding, setAdding] = useState(false);
  const [attempt, setAttempt] = useState(1);
  const delivery = simulation?.delivery;
  const mainJob: Job | undefined = delivery ? { id: deliveryTaskId, title: "Implement release checklist", requester: "Delivery workflow", priority: "High", attempt, branch: deliveryBranch, assignment: deliveryModels[0]?.selection, ...releaseQueue[delivery] } : undefined;
  const allJobs = mainJob ? [mainJob, ...jobs] : [];
  const visibleJobs = allJobs.filter((job) => matchesQueueView(job.state, view));
  const selectedJob = allJobs.find((job) => job.id === selected);
  const isPrimary = selectedJob?.id === deliveryTaskId;
  const active = allJobs.filter((job) => job.state === "Running").length;
  const next = isPrimary && delivery ? actions[delivery] : undefined;

  function advance(action: DeliveryAction) {
    if (!simulation) return;
    if (action === "resume" || action === "retry") setAttempt((current) => current + 1);
    simulation.advanceDelivery(action);
  }

  function addJob(title: string, assignment: ModelSelection) {
    const id = `job-${jobs.length + 1}`;
    setJobs((current) => [...current, { id, title, requester: "Operator", state: "Queued", priority: "Normal", reason: paused ? "Queue paused by the operator" : "Waiting for dispatch", attempt: 1, branch: "main", assignment }]);
    setSelected(id);
    setView("All");
    setAdding(false);
  }

  function setJobStatus(id: string, state: JobState, reason: string) {
    setJobs((items) => items.map((job) => job.id === id ? { ...job, state, reason } : job));
  }

  return <QueueLayout occupancy={simulation ? { active, capacity: 3 } : undefined} paused={paused} onClose={onClose}
    view={view} onViewChange={setView} onSelect={setSelected} onDeselect={() => setSelected("")}
    actions={<>
      <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" disabled={!simulation} onClick={() => setAdding(!adding)}><Plus size={14} /> New job</Button>
      <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" disabled={!simulation} onClick={() => setPaused(!paused)}>{paused ? <Play size={14} /> : <Pause size={14} />}{paused ? "Resume queue" : "Pause queue"}</Button>
    </>}
    controls={adding && <QueueJobForm onAdd={addJob} onCancel={() => setAdding(false)} />}
    state={!visibleJobs.length && <QueueEmpty filtered={allJobs.length > 0} />}
    jobs={visibleJobs.map(({ state, reason, ...job }) => ({
      ...job, status: state, reason: state === "Queued" && paused ? "Queue paused by the operator" : reason,
    }))}
    selected={selectedJob && {
      id: selectedJob.id, title: selectedJob.title, status: selectedJob.state,
      content: <>
        {["Interrupted", "Failed"].includes(selectedJob.state) && <InlineIssue>{selectedJob.reason}</InlineIssue>}
        <dl className="grid grid-cols-[4.75rem_minmax(0,1fr)] gap-1.5 text-sm">
          <dt className="text-muted-foreground">Requester</dt><dd>{selectedJob.requester}</dd>
          <dt className="text-muted-foreground">Worktree</dt><dd className="break-all font-mono text-xs">{selectedJob.branch}</dd>
          <dt className="text-muted-foreground">Model</dt><dd>{modelName(selectedJob.assignment ?? null)}</dd>
          <dt className="text-muted-foreground">Attempt</dt><dd>{selectedJob.attempt}</dd>
          <dt className="text-muted-foreground">Outcome</dt><dd>{selectedJob.reason}</dd>
        </dl>
        <div className="flex flex-wrap gap-2">
          {isPrimary && <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" onClick={() => openWorkspace("tasks", deliveryTaskId)}>Open task <ArrowRight size={14} /></Button>}
          {isPrimary && <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" onClick={() => openActivity(deliveryRunId)}>Inspect attempt</Button>}
          {isPrimary && (delivery === "question" || delivery === "plan") && <Button type="button" className="px-2.5 py-1.5 text-xs" onClick={() => openWorkspace("conversations", deliveryConversation)}><Clock3 size={14} /> {delivery === "question" ? "Answer question" : "Review plan"}</Button>}
          {next && <Button type="button" className="px-2.5 py-1.5 text-xs" disabled={next.action === "dispatch" && (paused || active >= 3)} onClick={() => advance(next.action)}>{next.action === "retry" ? <RotateCcw size={14} /> : <Play size={14} />}{next.title}</Button>}
          {isPrimary && delivery === "running" && <>
            <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" onClick={() => advance("interrupt")}><CircleStop size={14} /> Interrupt</Button>
            <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" onClick={() => advance("fail")}><AlertTriangle size={14} /> Simulate failure</Button>
          </>}
          {!isPrimary && selectedJob.state === "Queued" && <>
            <Button type="button" disabled={paused || active >= 3} className="px-2.5 py-1.5 text-xs" onClick={() => setJobStatus(selectedJob.id, "Running", "Execution slot assigned")}><Play size={14} /> Dispatch</Button>
            <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" onClick={() => setJobStatus(selectedJob.id, "Cancelled", "Cancelled before dispatch")}><X size={14} /> Cancel job</Button>
          </>}
          {!isPrimary && selectedJob.state === "Running" && <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" onClick={() => setJobStatus(selectedJob.id, "Completed", "Attempt finished successfully")}><CircleCheck size={14} /> Finish attempt</Button>}
        </div>
      </>,
    }}
  />;
}
