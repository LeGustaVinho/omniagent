import { invoke } from "@tauri-apps/api/core";
import { JobFailurePanel } from "@/job-failure";
import { useQuery } from "@tanstack/react-query";
import { CircleDot, FlaskConical, LibraryBig } from "lucide-react";
import { useState } from "react";
import type { RunRecord } from "@/bindings/RunRecord";
import type { CommandJobOutput } from "@/bindings/CommandJobOutput";
import { Button } from "@/components/ui/button";
import { ConversationActivity, ConversationRequests, StopConversation, isLiveRun, useConversationRun } from "@/conversation-run";
import { deliveryBase, deliveryBranch, deliveryNotePath, deliveryRunId, deliveryTaskId } from "@/tasks-demo-data";
import type { DeliveryStep, WorkspaceProps } from "@/workbench-contract";
import { WorktreeEvidence } from "@/worktree-evidence";

const steps: Record<DeliveryStep, string> = {
  note: "Ready to plan", question: "Waiting for input", plan: "Awaiting implementation approval",
  queued: "Queued", running: "Running", review: "In review", verified: "Verified", accepted: "Diff accepted",
  merged: "Merged", interrupted: "Interrupted", failed: "Failed",
};

const inspectorPages = ["Overview", "Timeline", "Agents", "Context", "Exact input", "Approvals", "Artifacts"] as const;
type InspectorPage = (typeof inspectorPages)[number];

type InspectorProps = WorkspaceProps & { runId: string; conversationId?: string; onClose: () => void };

export function RunInspector({ runId, conversationId, onClose, checkout, simulation, openWorkspace, openChanges }: InspectorProps) {
  const [page, setPage] = useState<InspectorPage>("Overview");
  if (!simulation) return <RecordedExecutionRun projectId={checkout.projectId} runId={runId}
    conversationId={conversationId} onClose={onClose} />;

  const step = simulation.delivery;
  const status = steps[step];
  const hasResult = ["review", "verified", "accepted", "merged"].includes(step);

  return (
    <section className="work-inspection">
      <div className="work-inspection-title"><span>Release checklist</span><code>{runId}</code></div>
      <div className="work-tag" role="status"><CircleDot size={13} /> {status}</div>
      <div className="work-inspection-tabs" role="group" aria-label="Run details">
        {inspectorPages.map((name) => <button key={name} type="button" aria-pressed={name === page} onClick={() => setPage(name)}>{name}</button>)}
      </div>
      <div className="work-inspection-body">
        {page === "Overview" && <><h3>Execution</h3><dl>
          <dt>Provider</dt><dd>Codex</dd><dt>Role</dt><dd>Implementation</dd><dt>Worktree</dt><dd><code>{deliveryBranch}</code></dd>
          <dt>Result</dt><dd>{hasResult ? "Candidate saved" : status}</dd>
        </dl><Button variant="outline" onClick={() => openWorkspace("tasks", deliveryTaskId)}>Open task</Button></>}
        {page === "Timeline" && <><h3>Events</h3><ol className="work-timeline">
          <li><span>Source selected</span><small>{deliveryNotePath} · {deliveryBase}</small></li>
          {step !== "note" && <li><span>Planning requested</span><small>Read-only profile · Codex</small></li>}
          <li><span>{status}</span><small>Latest state</small></li>
        </ol></>}
        {page === "Agents" && <><h3>Agent tree</h3><ul className="work-timeline">
          <li><span>Planning · Codex</span><small>Read-only workspace</small></li>
          {hasResult && <li className="ml-4"><span>Review · Claude Code</span><small>Independent verification</small></li>}
        </ul></>}
        {page === "Context" && <><h3>Context snapshot</h3><p>2,410 input tokens estimated</p>
          <button className="work-text-action" onClick={() => openWorkspace("knowledge", deliveryNotePath)}>{deliveryNotePath}</button>
          <p>Base commit <code>{deliveryBase}</code> · checkout instructions included</p></>}
        {page === "Exact input" && <><h3>Exact provider input</h3><pre>Use {deliveryNotePath} at {deliveryBase}. Keep the task scope limited to the approved changes and capture a diff for review.</pre></>}
        {page === "Approvals" && <><h3>Implementation release</h3><p>{["note", "question", "plan"].includes(step) ? "A decision is required for the selected plan." : "Plan revision 1 · approved example scope"}</p>
          <Button variant="outline" onClick={() => openWorkspace("conversations", "plan")}>Open conversation</Button></>}
        {page === "Artifacts" && <><h3>Artifacts</h3>{hasResult
          ? <Button variant="outline" onClick={() => openChanges("saved")}>Open saved diff</Button>
          : <p>Artifacts appear after the candidate is captured.</p>}</>}
      </div>
    </section>
  );
}

type RecordedRunProps = Pick<InspectorProps, "conversationId" | "runId" | "onClose"> & { projectId: string };

function RecordedExecutionRun(props: RecordedRunProps) {
  const { projectId, conversationId, runId } = props;
  const command = useQuery({
    queryKey: ["command-job-run", projectId, runId],
    queryFn: () => invoke<RunRecord | null>("command_job_run", { projectId, runId }),
    enabled: conversationId === undefined,
    refetchInterval: (query) => query.state.data && isLiveRun(query.state.data) ? 400 : false,
  });
  if (conversationId) return <RecordedConversationRun {...props} />;
  if (command.isError) return <p className="work-progress" role="alert">{String(command.error)}</p>;
  if (command.isPending) return <p className="work-progress" role="status">Loading execution…</p>;
  return command.data ? <RecordedCommandRun run={command.data} onClose={props.onClose} />
    : <RecordedConversationRun {...props} />;
}

function RecordedCommandRun({ run, onClose }: { run: RunRecord; onClose: () => void }) {
  const { id: runId, projectId, status } = run;
  const [open, setOpen] = useState(false);
  const live = isLiveRun(run);
  const outputs = useQuery({
    queryKey: ["command-job-output", projectId, runId],
    queryFn: () => invoke<CommandJobOutput | null>("command_job_output", { projectId, runId }),
    enabled: open && !live,
    staleTime: Infinity,
  });
  const failure = status === "failed" || status === "interrupted";
  return <section className="work-inspection" aria-label="Command activity">
    <div className="work-inspection-title"><span>Command attempt</span><code>{runId}</code>
      <Button variant="outline" onClick={onClose}>Close</Button>
    </div>
    <p className="work-tag capitalize" role="status">{status.replaceAll("_", " ")}</p>
    <div className="work-inspection-body">
      {failure && <JobFailurePanel projectId={projectId} runId={runId} />}
      {!live && !failure && <WorktreeEvidence projectId={projectId} runId={runId} ownerTaskId={null} />}
      {!live && <details onToggle={(event) => setOpen(event.currentTarget.open)}>
        <summary>Command output</summary>
        {open && <>
          {outputs.isPending && <p role="status">Loading command output…</p>}
          {outputs.isError && <p role="alert">{String(outputs.error)}</p>}
          {outputs.data === null && <p>Command output is unavailable.</p>}
          {outputs.data && <>
            {outputs.data.process && <p>Exit code {outputs.data.process.code}</p>}
            {outputs.data.truncated && <p>Output capture is incomplete.</p>}
            {outputs.data.stdout && <><h3>Standard output</h3><pre>{outputs.data.stdout}</pre></>}
            {outputs.data.stderr && <><h3>Standard error</h3><pre>{outputs.data.stderr}</pre></>}
          </>}
        </>}
      </details>}
    </div>
  </section>;
}

function RecordedConversationRun({ conversationId, projectId, runId, onClose }: RecordedRunProps) {
  const current = useQuery({
    queryKey: ["inspect-conversation-run", projectId, conversationId, runId],
    queryFn: () => invoke<[RunRecord, string]>("inspect_conversation_run", { projectId, conversationId, runId }),
    refetchInterval: (query) => isLiveRun(query.state.data?.[0]) ? 400 : false,
  });
  const [run, owner] = current.data ?? [];
  const activity = useConversationRun(owner ?? "", run, false);
  if (current.error) return <p className="work-progress" role="alert">{String(current.error)}</p>;
  if (!run || !owner) return <p className="work-progress" role="status">Loading execution…</p>;
  return <div className="h-full overflow-auto">
    <ConversationActivity embedded conversationId={owner} run={run} projection={activity.projection} onClose={onClose} />
    <div className="work-inspection-body">
      {(run.status === "failed" || run.status === "interrupted") && <JobFailurePanel projectId={projectId} runId={runId} />}
      <ConversationRequests conversationId={owner} runId={runId} activity={activity} />
      {activity.live && <StopConversation {...activity} />}
      {activity.error && <p role="alert">{String(activity.error)}</p>}
    </div>
  </div>;
}

export function SimulationDiagnostics({ openSystem, openActivity, simulation }: WorkspaceProps & { openSystem?: () => void }) {
  const [tab, setTab] = useState<"desktop" | "fixture">("desktop");
  const [fixtureStatus, setFixtureStatus] = useState<"Idle" | "Waiting for input" | "Succeeded" | "Interrupted">("Idle");
  const [input, setInput] = useState("");

  return <section className="work-page" aria-label="Diagnostics workspace">
    <header className="work-page-heading"><div><h1>Diagnostics</h1><p>Desktop runtime and fixture checks</p></div></header>
    <div className="work-inspection-tabs" role="group" aria-label="Diagnostic views">
      <button type="button" aria-pressed={tab === "desktop"} onClick={() => setTab("desktop")}>Desktop</button>
      <button type="button" aria-pressed={tab === "fixture"} onClick={() => setTab("fixture")}>Fixture runs</button>
    </div>
    {tab === "desktop" ? <div className="work-diagnostic-grid">
      <section className="work-stat"><FlaskConical size={20} /><h2>Windows qualification</h2><p>Review provider, WebView2 and installer status for this scenario.</p>
        <Button variant="outline" onClick={openSystem}>System settings</Button></section>
      <section className="work-stat"><LibraryBig size={20} /><h2>Recorded attempt</h2><p>{simulation && steps[simulation.delivery]}</p>
        <Button variant="outline" onClick={() => openActivity(deliveryRunId)}>Open run inspector</Button></section>
    </div> : <section className="work-stat">
      <h2>Process fixture</h2><p role="status">{fixtureStatus}</p>
      <div className="flex flex-wrap items-center gap-2">
        <Button onClick={() => { setFixtureStatus("Waiting for input"); setInput(""); }}>Start fixture</Button>
        {fixtureStatus === "Waiting for input" && <Button variant="outline" onClick={() => setFixtureStatus("Interrupted")}>Cancel run</Button>}
      </div>
      {fixtureStatus === "Waiting for input" && <form className="flex flex-wrap items-end gap-2" onSubmit={(event) => { event.preventDefault(); setFixtureStatus("Succeeded"); }}>
        <label className="grid gap-1">Fixture input<input value={input} onChange={(event) => setInput(event.target.value)} /></label>
        <Button type="submit" disabled={!input.trim()}>Send input</Button>
      </form>}
      {fixtureStatus === "Succeeded" && <pre className="work-fixture-output">Input received: {input}</pre>}
    </section>}
  </section>;
}
