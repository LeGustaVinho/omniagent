import { AlertTriangle, ArrowRight, CircleCheck, CircleStop, Clock3, Play } from "lucide-react";
import { useState } from "react";
import { deliveryConversation } from "@/conversation-preview-data";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import type { SessionMode } from "@/bindings/SessionMode";
import { deliveryBranch, deliveryModels, deliveryRunId, deliveryTaskId, modelName } from "@/tasks-demo-data";
import LiveQueue from "@/queue-live";
import {
  AddQueueJobButton, PendingQueueJobActions, QueueButton, QueueEmpty, QueueJobActions, QueueJobFacts, QueueJobForm,
  QueueLayout, QueuePauseButton, QueueRecoveryButton, type QueueJobDraft,
} from "@/queue-layout";
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
  mode: SessionMode;
  assignment?: ModelSelection | null;
}

const auxiliaryJobs: Job[] = [
  { id: "docs", title: "Index repository notes", requester: "Knowledge", state: "Queued", priority: "Normal", reason: "Waiting for the execution slot", attempt: 1, branch: "main", mode: "read_only", assignment: deliveryModels[2]?.selection },
  { id: "bootstrap", title: "Check direct runner", requester: "Diagnostics", state: "Completed", priority: "Normal", reason: "Runner replied successfully", attempt: 1, branch: "main", mode: "read_only", assignment: deliveryModels[0]?.selection },
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
};

function SimulatedJobForm({ parent, onAdd, onCancel }: { parent?: Job; onAdd: (draft: QueueJobDraft) => void; onCancel: () => void }) {
  const [provider, setProvider] = useState<Provider>("codex");
  return <QueueJobForm provider={provider} onProvider={setProvider}
    options={deliveryModels.filter(({ selection }) => selection.provider === provider)} child={Boolean(parent)}
    allowWrite={!parent || parent.mode === "approved_write"} onSubmit={onAdd} onCancel={onCancel} />;
}

export default function QueuePanel(props: WorkspaceProps & { onClose?: () => void; active?: boolean; focusJob?: QueueJobFocus }) {
  return props.simulation ? <SimulatedQueue {...props} /> : <LiveQueue {...props} />;
}

function SimulatedQueue({ simulation, openWorkspace, openActivity, onClose }: WorkspaceProps & { onClose?: () => void }) {
  const [paused, setPaused] = useState(false);
  const [view, setView] = useState<QueueView>("All");
  const [jobs, setJobs] = useState(auxiliaryJobs);
  const [selected, setSelected] = useState<string>(deliveryTaskId);
  const [adding, setAdding] = useState<{ parent?: Job }>();
  const [attempt, setAttempt] = useState(1);
  const delivery = simulation?.delivery;
  const mainJob: Job | undefined = delivery ? {
    id: deliveryTaskId, title: "Implement release checklist", requester: "Delivery workflow", priority: "High", attempt,
    branch: deliveryBranch, mode: "approved_write", assignment: deliveryModels[0]?.selection, ...releaseQueue[delivery],
  } : undefined;
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

  function addJob({ instructions, selection, mode }: QueueJobDraft) {
    const id = `job-${jobs.length + 1}`;
    const title = instructions.split(/\r?\n/, 1)[0]!.trimEnd();
    const parent = adding?.parent;
    setJobs((current) => [...current, {
      id, title, requester: parent ? `Operator · parent job ${parent.title}` : "Operator", state: "Queued", priority: "Normal",
      reason: "Waiting for dispatch", attempt: 1, branch: parent?.branch ?? "main", mode, assignment: selection,
    }]);
    setSelected(id);
    setView("All");
    setAdding(undefined);
  }

  function setJobStatus(id: string, state: JobState, reason: string) {
    setJobs((items) => items.map((job) => job.id === id ? { ...job, state, reason } : job));
  }

  const needsAttention = allJobs.filter((job) => matchesQueueView(job.state, "Needs input")).length;

  return <QueueLayout occupancy={simulation ? { active, capacity: 3 } : undefined} paused={paused} onClose={onClose}
    needsAttention={needsAttention}
    view={view} onViewChange={setView} onSelect={setSelected} onDeselect={() => setSelected("")}
    actions={<>
      <AddQueueJobButton disabled={!simulation || Boolean(adding)} onClick={() => setAdding({})} />
      <QueuePauseButton paused={paused} disabled={!simulation} onClick={() => setPaused(!paused)} />
    </>}
    controls={adding && <SimulatedJobForm key={adding.parent?.id ?? "job"} parent={adding.parent}
      onAdd={addJob} onCancel={() => setAdding(undefined)} />}
    state={!visibleJobs.length && <QueueEmpty filtered={allJobs.length > 0} />}
    jobs={visibleJobs.map(({ state, reason, ...job }) => ({
      ...job, status: state, reason: state === "Queued" && paused ? "Queue paused by the operator" : reason,
    }))}
    selected={selectedJob && {
      id: selectedJob.id, title: selectedJob.title, status: selectedJob.state,
      content: <>
        {["Interrupted", "Failed"].includes(selectedJob.state) && <InlineIssue>{selectedJob.reason}</InlineIssue>}
        <QueueJobFacts facts={[
          { label: "Job ID", value: selectedJob.id, mono: true },
          { label: "Requester", value: selectedJob.requester },
          { label: "Worktree", value: selectedJob.branch, mono: true },
          { label: "Profile mode", value: selectedJob.mode.replaceAll("_", " ") },
          { label: "Priority", value: selectedJob.priority },
          { label: "Model", value: modelName(selectedJob.assignment ?? null) },
          { label: "Attempt", value: `#${selectedJob.attempt}` },
          { label: "Outcome", value: selectedJob.reason },
        ]} />
        <QueueJobActions>
          {isPrimary && <QueueButton variant="outline" onClick={() => openWorkspace("tasks", deliveryTaskId)}>Open task <ArrowRight size={13} aria-hidden /></QueueButton>}
          {isPrimary && <QueueButton variant="outline" onClick={() => openActivity(deliveryRunId)}>Inspect attempt</QueueButton>}
          {isPrimary && (delivery === "question" || delivery === "plan") && <QueueButton onClick={() => openWorkspace("conversations", deliveryConversation)}><Clock3 size={14} aria-hidden /> {delivery === "question" ? "Answer question" : "Review plan"}</QueueButton>}
          {next && <QueueButton disabled={next.action === "dispatch" && (paused || active >= 3)} onClick={() => advance(next.action)}><Play size={14} aria-hidden /> {next.title}</QueueButton>}
          {isPrimary && delivery === "interrupted" && <QueueRecoveryButton action="resume" onClick={() => advance("resume")} />}
          {isPrimary && delivery === "failed" && <QueueRecoveryButton action="retry" onClick={() => advance("retry")} />}
          {isPrimary && delivery === "running" && <>
            <QueueButton variant="outline" onClick={() => advance("interrupt")}><CircleStop size={14} aria-hidden /> Interrupt</QueueButton>
            <QueueButton variant="outline" onClick={() => advance("fail")}><AlertTriangle size={14} aria-hidden /> Simulate failure</QueueButton>
          </>}
          {selectedJob.state === "Running" && <QueueButton variant="outline" onClick={() => setAdding({ parent: selectedJob })}>Add subagent</QueueButton>}
          {!isPrimary && (selectedJob.state === "Queued" || selectedJob.state === "Paused") && <PendingQueueJobActions
            paused={selectedJob.state === "Paused"}
            onPauseToggle={() => selectedJob.state === "Paused" ? setJobStatus(selectedJob.id, "Queued", "Waiting for dispatch")
              : setJobStatus(selectedJob.id, "Paused", "This job is paused.")}
            onCancel={() => setJobStatus(selectedJob.id, "Cancelled", "Cancelled before dispatch")} />}
          {!isPrimary && selectedJob.state === "Queued" && <QueueButton disabled={paused || active >= 3}
            onClick={() => setJobStatus(selectedJob.id, "Running", "Execution slot assigned")}><Play size={14} aria-hidden /> Dispatch</QueueButton>}
          {!isPrimary && selectedJob.state === "Running" && <>
            <QueueButton variant="outline" onClick={() => setJobStatus(selectedJob.id, "Interrupted", "Attempt interrupted by the operator")}><CircleStop size={14} aria-hidden /> Interrupt attempt</QueueButton>
            <QueueButton variant="outline" onClick={() => setJobStatus(selectedJob.id, "Completed", "Attempt finished successfully")}><CircleCheck size={14} aria-hidden /> Finish attempt</QueueButton>
          </>}
          {!isPrimary && selectedJob.state === "Interrupted" && <QueueRecoveryButton action="resume"
            onClick={() => setJobStatus(selectedJob.id, "Queued", "Checkpoint queued for resumption.")} />}
        </QueueJobActions>
      </>,
    }}
  />;
}
