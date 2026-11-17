import { invoke } from "@tauri-apps/api/core";
import { JobFailurePanel } from "@/job-failure";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { DesktopDiagnostics, DiagnosticsWorkspace, type DiagnosticView } from "@/diagnostics-workspace";
import { SimulationFixture } from "@/fixture-panels";
import type { RunRecord } from "@/bindings/RunRecord";
import type { CommandJobOutput } from "@/bindings/CommandJobOutput";
import type { DownstreamWait } from "@/bindings/DownstreamWait";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { ConversationChecklistProgress } from "@/conversation-checklist-progress";
import { ConversationApprovalEvidence, ObservedConversationResponse } from "@/conversation-response-evidence";
import {
  ConversationActivityAgents, ConversationActivityContext, ConversationActivityTimeline, ConversationExactInput,
  ConversationRequests, StopConversation, conversationRunStatus, isLiveRun, useConversationRun,
} from "@/conversation-run";
import { deliveryBase, deliveryBranch, deliveryNotePath, deliveryRunId, deliveryTaskId } from "@/tasks-demo-data";
import type { DeliveryStep, QuestionFocus, WorkspaceProps } from "@/workbench-contract";
import { RunArtifactEvidence, RunOrchestrationPanel } from "@/run-orchestration";
import { RunPlanHistory } from "@/run-plan-history";
import { RunInspectionLayout } from "@/run-inspection-layout";
import { RunInspectorNavigation } from "@/run-inspector-navigation";
import { SavedRunOutputComparison } from "@/saved-attempt-comparison";

const steps: Record<DeliveryStep, string> = {
  note: "Ready to plan", question: "Waiting for input", plan: "Awaiting implementation approval",
  queued: "Queued", running: "Running", review: "In review", verified: "Verified", accepted: "Diff accepted",
  merged: "Merged", interrupted: "Interrupted", failed: "Failed",
};

type InspectorProps = WorkspaceProps & {
  runId: string; conversationId?: string; focusQuestion?: QuestionFocus; onClose: () => void;
};

export function RunInspector({
  runId, conversationId, focusQuestion, onClose, checkout, simulation, openWorkspace, openChanges, openActivity,
}: InspectorProps) {
  if (!simulation) return <RecordedExecutionRun projectId={checkout.projectId} checkoutId={checkout.checkoutId} runId={runId}
    conversationId={conversationId} focusQuestion={focusQuestion}
    onClose={onClose} openActivity={openActivity} openWorkspace={openWorkspace} />;

  const step = simulation.delivery;
  const status = steps[step];
  const hasResult = ["review", "verified", "accepted", "merged"].includes(step);

  return <RunInspectionLayout title="Release checklist" runId={runId} status={{ label: status }} pages={[
    { id: "overview", name: "Overview", content: <><h3>Execution</h3><dl>
      <dt>Provider</dt><dd>Codex</dd><dt>Role</dt><dd>Implementation</dd><dt>Worktree</dt><dd><code>{deliveryBranch}</code></dd>
      <dt>Result</dt><dd>{hasResult ? "Candidate saved" : status}</dd>
    </dl><Button variant="outline" onClick={() => openWorkspace("tasks", deliveryTaskId)}>Open task</Button></> },
    { id: "timeline", name: "Timeline", content: <><h3>Events</h3><ol className="work-timeline">
      <li><span>Source selected</span><small>{deliveryNotePath} · {deliveryBase}</small></li>
      {step !== "note" && <li><span>Planning requested</span><small>Read-only profile · Codex</small></li>}
      <li><span>{status}</span><small>Latest state</small></li>
    </ol></> },
    { id: "agents", name: "Agents", content: <><h3>Agent tree</h3><ul className="work-timeline">
      <li><span>Planning · Codex</span><small>Read-only workspace</small></li>
      {hasResult && <li className="ml-4"><span>Review · Claude Code</span><small>Independent verification</small></li>}
    </ul></> },
    { id: "context", name: "Context", content: <><h3>Context snapshot</h3><p>2,410 input tokens estimated</p>
      <button type="button" className="work-text-action" onClick={() => openWorkspace("knowledge", deliveryNotePath)}>{deliveryNotePath}</button>
      <p>Base commit <code>{deliveryBase}</code> · checkout instructions included</p></> },
    { id: "input", name: "Exact input", content: <><h3>Exact provider input</h3>
      <pre>Use {deliveryNotePath} at {deliveryBase}. Keep the task scope limited to the approved changes and capture a diff for review.</pre></> },
    { id: "approvals", name: "Approvals", content: <><h3>Implementation release</h3><p>{["note", "question", "plan"].includes(step) ? "A decision is required for the selected plan." : "Plan revision 1 · approved example scope"}</p>
      <Button variant="outline" onClick={() => openWorkspace("conversations", "plan")}>Open conversation</Button></> },
    { id: "artifacts", name: "Artifacts", content: <><h3>Artifacts</h3>{hasResult
      ? <Button variant="outline" onClick={() => openChanges("saved")}>Open saved diff</Button>
      : <p>Artifacts appear after the candidate is captured.</p>}</> },
  ]} />;
}

type RecordedRunProps = Pick<InspectorProps, "conversationId" | "runId" | "focusQuestion" | "onClose" | "openActivity" | "openWorkspace"> & { projectId: string; checkoutId: string };

function RecordedExecutionRun(props: RecordedRunProps) {
  const { projectId, conversationId, runId } = props;
  const command = useQuery({
    queryKey: ["command-job-run", projectId, runId],
    queryFn: () => invoke<RunRecord | null>("command_job_run", { projectId, runId }),
    enabled: conversationId === undefined,
    refetchInterval: (query) => query.state.data && isLiveRun(query.state.data) ? 400 : false,
  });
  if (conversationId) return <RecordedConversationRun {...props} />;
  if (command.isError || command.isPending) return <RunInspectionLayout title="Execution" runId={runId}>
    {command.isError ? <p role="alert">{String(command.error)}</p> : <p role="status">Loading execution…</p>}
  </RunInspectionLayout>;
  return command.data ? <RecordedCommandRun key={JSON.stringify([projectId, runId])} run={command.data} onClose={props.onClose} navigation={props} />
    : <RecordedConversationRun {...props} />;
}

function RecordedCommandRun({ run, onClose, navigation }: {
  run: RunRecord; onClose: () => void;
  navigation: Pick<WorkspaceProps, "openActivity" | "openWorkspace">;
}) {
  const { id: runId, projectId, status } = run;
  const live = isLiveRun(run);
  const failure = status === "failed" || status === "interrupted";
  const overview = <>
    {failure && <JobFailurePanel projectId={projectId} runId={runId} />}
    <RunOrchestrationPanel scope={{ projectId, runId }} live={live} navigation={navigation} />
  </>;

  return <RunInspectionLayout label="Command activity" title="Command attempt" runId={runId}
    status={{ label: status[0]!.toUpperCase() + status.slice(1).replaceAll("_", " "),
      tone: failure ? "danger" : status === "succeeded" ? "success" : live ? "progress" : undefined }}
    actions={<Button type="button" variant="ghost" onClick={onClose}>Close</Button>}
    pages={live ? undefined : [
      { id: "overview", name: "Overview", content: overview },
      { id: "output", name: "Command output", content: <RecordedCommandOutput projectId={projectId} runId={runId} openActivity={navigation.openActivity} /> },
    ]}>
    {overview}
  </RunInspectionLayout>;
}

function RecordedCommandOutput({ projectId, runId, openActivity }: Pick<RecordedRunProps, "projectId" | "runId" | "openActivity">) {
  const { copy, feedback } = useContextActionFeedback();
  const [compare, setCompare] = useState(false);
  const outputs = useQuery({
    queryKey: ["command-job-output", projectId, runId],
    queryFn: () => invoke<CommandJobOutput | null>("command_job_output", { projectId, runId }),
    staleTime: Infinity,
  });
  if (outputs.isPending) return <p role="status">Loading command output…</p>;
  if (outputs.isError) return <p role="alert">{String(outputs.error)}</p>;
  if (!outputs.data) return <p>Command output is unavailable.</p>;
  const { process, truncated, stdout, stderr } = outputs.data;
  const streams = [
    { name: "Standard output", text: stdout },
    { name: "Standard error", text: stderr },
  ].filter(({ text }) => text);
  const compareOutput = { label: "Compare saved attempt output…", run: () => setCompare(true) };
  return <>
    {process && <ContextActions label="Command exit code" groups={[[compareOutput], [{ label: "Copy exit code", run: () => copy(String(process.code)) }]]}>
      <p>Exit code {process.code}</p>
    </ContextActions>}
    {truncated && <ContextActions label="Output capture" groups={process || streams.length ? [] : [[compareOutput]]}>
      <p>Output capture is incomplete.</p>
    </ContextActions>}
    {streams.map(({ name, text }) => <ContextActions key={name} label={name} groups={[[
      compareOutput,
    ], [
      { label: `Copy ${name.toLowerCase()}`, run: () => copy(text) },
      ...(streams.length > 1 ? [{ label: "Copy captured output", run: () => copy(
        [process && `Exit code ${process.code}`, truncated && "Output capture is incomplete.",
          ...streams.map(({ name, text }) => `${name}\n${text}`)].filter(Boolean).join("\n\n"),
      ) }] : []),
    ]]}>
      <div className="min-w-0 flex-1"><h3>{name}</h3><pre>{text}</pre></div>
    </ContextActions>)}
    {!process && !truncated && !stdout && !stderr && <p>No output was captured.</p>}
    {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
    {compare && <SavedRunOutputComparison projectId={projectId} runId={runId} close={() => setCompare(false)} openActivity={openActivity} />}
  </>;
}

function RecordedConversationRun({ conversationId, projectId, checkoutId, runId, focusQuestion, onClose, openActivity, openWorkspace }: RecordedRunProps) {
  const { copy, feedback, run: perform } = useContextActionFeedback();
  const [comparedRun, setComparedRun] = useState<string>();
  const current = useQuery({
    queryKey: ["inspect-conversation-run", projectId, conversationId, runId],
    queryFn: () => invoke<[RunRecord, string]>("inspect_conversation_run", { projectId, conversationId, runId }),
    refetchInterval: (query) => isLiveRun(query.state.data?.[0]) ? 400 : false,
  });
  const [run, owner] = current.data ?? [];
  const activity = useConversationRun(owner ?? "", run, false);
  if (current.error || !run || !owner) return <RunInspectionLayout title="Conversation attempt" runId={runId}>
    {current.error ? <p role="alert">{String(current.error)}</p> : <p role="status">Loading execution…</p>}
  </RunInspectionLayout>;
  const { live, projection } = activity;
  const scope = { conversationId: owner, runId, live };
  const attempt = { projectId, conversationId: owner, runId };
  const navigation = { openActivity, openWorkspace };
  const showNotice = activity.inputs.length > 0 || activity.savedQuestions.length > 0 || Boolean(activity.error);
  return <RunInspectionLayout key={run.id} as="aside" label="Conversation activity" title="Conversation attempt"
    runId={run.id} status={conversationRunStatus(run)}
    menu={[[
      ...(run.checkoutId === checkoutId ? [{ label: "Open conversation", run: () => void perform(async () => {
        await invoke("checkout_conversation", { checkoutId, conversationId: owner });
        openWorkspace("conversations", owner);
        onClose();
      }) }] : []),
      { label: "Copy conversation ID", run: () => copy(owner) },
      { label: "Copy attempt checkout ID", run: () => copy(run.checkoutId) },
    ]]}
    actions={live && <StopConversation {...activity} />}
    notice={(showNotice || feedback) && <>
      <ConversationRequests conversationId={owner} runId={runId} activity={activity} focusQuestion={focusQuestion} />
      {activity.error && <p role="alert">{String(activity.error)}</p>}
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
    </>}
    pages={[
      { id: "overview", name: "Overview", content: <>
        {(run.status === "failed" || run.status === "interrupted") && <JobFailurePanel projectId={projectId} runId={runId} />}
        <RunOrchestrationPanel scope={attempt} live={live} navigation={navigation} showArtifacts={false} />
        <ConversationChecklistProgress projectId={projectId} runId={runId} live={live} />
        {!live && <DownstreamHistory projectId={projectId} runId={runId} openActivity={openActivity} />}
      </> },
      { id: "timeline", name: "Timeline", content: <>
        <h3>Events</h3>
        <ConversationActivityTimeline {...scope} projection={projection} />
        <RunPlanHistory projectId={projectId} runId={runId} openActivity={openActivity} />
      </> },
      { id: "agents", name: "Agents", content: <ConversationActivityAgents {...scope} projection={projection} openActivity={openActivity} /> },
      { id: "context", name: "Context", content: <ConversationActivityContext {...scope} projectId={projectId} projection={projection} /> },
      { id: "input", name: "Exact input", content: <ConversationExactInput {...scope} checkoutId={run.checkoutId} /> },
      { id: "approvals", name: "Approvals", content: <><h3>Approval history</h3><ConversationApprovalEvidence {...scope} /></> },
      { id: "artifacts", name: "Artifacts", content: <>
        <ContextActions label="Recorded output" groups={!live && Boolean(projection?.responses.items.length)
          ? [[{ label: "Compare saved attempt output…", run: () => setComparedRun(runId) }]] : []}>
          <h3>Recorded output</h3>
        </ContextActions>
        <RunInspectorNavigation.Provider value={{ ...navigation, activeRunId: runId, openGitAttempt: (id) => {
          openWorkspace("git", `attempt:${id}`);
          onClose();
        } }}>
          <RunArtifactEvidence scope={attempt} live={live} navigation={navigation} />
        </RunInspectorNavigation.Provider>
        {projection && <ObservedConversationResponse evidence={projection} />}
        {projection && !projection.responses.items.length && <p>No response text recorded.</p>}
        {comparedRun === runId && <SavedRunOutputComparison projectId={projectId} conversationId={owner} runId={runId}
          close={() => setComparedRun(undefined)} openActivity={openActivity} />}
      </> },
    ]} />;
}

function DownstreamHistory({ projectId, runId, openActivity }: Pick<RecordedRunProps, "projectId" | "runId" | "openActivity">) {
  const { copy, feedback } = useContextActionFeedback();
  const recorded = useQuery({
    queryKey: ["downstream-wait", projectId, runId],
    queryFn: () => invoke<DownstreamWait | null>("downstream_wait", { projectId, runId }),
    refetchInterval: ({ state }) => state.data?.state === "waiting" ? 750 : false,
  });
  if (recorded.isError) return <p role="alert">{String(recorded.error)}</p>;
  const wait = recorded.data;
  if (!wait) return null;
  return <section aria-label="Downstream results" className="run-inspection-attempts grid gap-2 pb-3">
    <h3>Downstream results</h3>
    <p className="capitalize">{wait.state.replaceAll("_", " ")}</p>
    <ul className="grid gap-1">
      {wait.inputs.map(({ childJobId, childRunId, status }) => <li key={childJobId}>
        <ContextActions label={`Downstream job ${childJobId}`} groups={[[
          ...(childRunId ? [{ label: "Inspect child attempt", run: () => openActivity(childRunId) }] : []),
        ], [
          { label: "Copy child job ID", run: () => copy(childJobId) },
          ...(childRunId ? [{ label: "Copy child run ID", run: () => copy(childRunId) }] : []),
        ]]}>
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <code className="break-all">{childJobId}</code>
            <span className="capitalize">{status ?? "Pending result"}</span>
            {childRunId && <Button type="button" variant="outline" onClick={() => openActivity(childRunId)}>Inspect child attempt</Button>}
          </div>
        </ContextActions>
      </li>)}
    </ul>
    {wait.continuationRunId && <Button className="justify-self-start" variant="outline" onClick={() => openActivity(wait.continuationRunId!)}>
      Open continuation
    </Button>}
    {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
  </section>;
}

export function SimulationDiagnostics({ openSystem, openActivity, simulation }: WorkspaceProps & { openSystem?: () => void }) {
  const [view, setView] = useState<DiagnosticView>("desktop");
  return <DiagnosticsWorkspace active={view} onSelect={setView} fixtures={<SimulationFixture />}
    desktop={<DesktopDiagnostics openSystem={openSystem} openAttempt={() => openActivity(deliveryRunId)}
      installation={<dl className="settings-facts">
        <dt>Runtime</dt><dd>Windows 11 · WebView2</dd>
        {simulation && <><dt>Recorded attempt</dt><dd>{steps[simulation.delivery]}</dd></>}
      </dl>}
      executables={<dl className="settings-facts">
        <dt>Git for Windows</dt><dd>Available in this scenario</dd>
        <dt>Codex</dt><dd>Example executable</dd>
      </dl>} />} />;
}
