import { ArrowUpRight, GitBranch, UsersRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  controlForNode, executionOutputForWire, workflowRunTarget,
  type ExecutionUnit, type WorkflowExecutionSnapshot,
} from "@/workflow-execution-model";
import { WorkflowExecutionStateBadge } from "@/workflow-execution-state";
import type { WorkspaceProps } from "@/workbench-contract";

type RunNavigation = Pick<WorkspaceProps, "openActivity" | "openWorkspace" | "openQueueJob">;
type PromptDecision = { route: string; reason: string; selected: Record<string, string> };
type PromptCall = {
  role: string;
  focus?: string | null;
  status: "completed" | "failed" | "skipped";
  artifactId?: string | null;
  stageId: string;
  attemptId?: string | null;
};
type PromptOutcome = PromptDecision & { calls: PromptCall[]; diagnostic?: string | null };

const routeName = (route: string) => ({
  concluido: "Completed", falhou: "Failed",
})[route] ?? route.replace(/[_-]/g, " ");

export function WorkflowPromptRunPanel({ snapshot, nodeId, unit, navigation }: {
  snapshot: WorkflowExecutionSnapshot; nodeId: string; unit?: ExecutionUnit; navigation: RunNavigation;
}) {
  const scope = snapshot.definition.subflows?.findLast(({ path, nodes, outputs }) =>
    Object.hasOwn(outputs, "falhou") && nodes.includes(nodeId) && nodes.includes(path + "::execute"));
  const failurePort = scope?.outputs.falhou;
  if (!scope || !failurePort) return null;
  const evidence = (port: "decision" | "outcome") => scope.outputs[port] && executionOutputForWire(snapshot, scope.outputs[port], unit)?.artifact;
  const decisionArtifact = evidence("decision");
  const outcomeArtifact = evidence("outcome");
  const outcome = outcomeArtifact?.value.evidence as PromptOutcome | undefined;
  const decision = decisionArtifact?.value.evidence as PromptDecision | undefined;
  const failure = controlForNode(snapshot, failurePort.node, unit);
  const detail = controlForNode(snapshot, scope.path + "::decision", unit);
  const route = outcome?.route ?? (failure?.selectedPorts.includes(failurePort.port) ? "falhou" : undefined)
    ?? Object.entries(scope.outputs).find(([port, endpoint]) => !["decision", "outcome", "evidence"].includes(port)
      && controlForNode(snapshot, endpoint.node, unit)?.selectedPorts.includes(endpoint.port))?.[0];
  const executor = controlForNode(snapshot, scope.path + "::execute", unit);
  const preparation = controlForNode(snapshot, scope.path + "::instructions", unit);
  const diagnostic = outcome?.diagnostic ?? [detail, executor, preparation].find((receipt) => receipt?.state === "failed")?.reason;
  const calls: PromptCall[] = outcome?.calls ?? Object.entries(decision?.selected ?? {}).map(([role, focus]) => ({
    role, focus, stageId: scope.path + "::delegate-" + role, status: "completed",
  }));
  if (!route && !decision && !diagnostic) return null;
  const status = route === "falhou" ? "failed" : route ? "completed" : "waiting";
  const jobFor = (call: PromptCall) => snapshot.jobs.find(({ nodeId, attempts, unitId }) =>
    nodeId === call.stageId && (!snapshot.unitNodeIds.includes(nodeId) || unit?.unit.id === unitId)
      && (!call.attemptId || attempts.some(({ runId }) => runId === call.attemptId)));
  return <section className="workflow-record-card pipeline-stack" aria-label="Prompt route and invited Roles">
    <header className="flex flex-wrap items-center justify-between gap-2">
      <h3 className="flex items-center gap-1.5"><GitBranch size={15} aria-hidden /> {scope.name}</h3>
      <WorkflowExecutionStateBadge status={status} />
    </header>
    {(route || decision) && <p><strong>{route ? "Route" : "Proposed route"}</strong> · {routeName(route ?? decision!.route)}</p>}
    {(outcome?.reason ?? decision?.reason) && <p>{outcome?.reason ?? decision?.reason}</p>}
    {diagnostic && <p role="status">{diagnostic}</p>}
    {calls.length > 0 && <section className="pipeline-stack">
      <h4 className="flex items-center gap-1.5"><UsersRound size={14} aria-hidden /> Invited Roles</h4>
      <ul className="pipeline-stack">{calls.map((call) => {
        const savedJob = jobFor(call);
        const attempt = savedJob?.attempts.find(({ runId }) => runId === call.attemptId) ?? savedJob?.attempts.at(-1);
        const label = snapshot.rolePins?.find(({ nodeId }) => nodeId === call.stageId)?.role?.name ?? call.role;
        return <li key={call.role} className="workflow-prompt-settings-row">
          <div><strong>{label}</strong> · {outcome
            ? { completed: "Completed", failed: "Failed", skipped: "Skipped" }[call.status] : attempt?.status === "running" ? "Running" : "Selected"}</div>
          {call.focus && <p className="col-span-full">{call.focus}</p>}
          {(attempt || call.artifactId || (navigation.openQueueJob && savedJob)) && <div className="pipeline-actions col-span-full">
            {attempt && <Button type="button" variant="ghost" onClick={() =>
              navigation.openActivity(attempt.runId, attempt.conversationId ?? undefined)}>Open attempt <ArrowUpRight size={13} aria-hidden /></Button>}
            {call.artifactId && <Button type="button" variant="ghost" onClick={() =>
              navigation.openWorkspace("pipelines", workflowRunTarget(snapshot.run.id, { nodeId: call.stageId, artifactId: call.artifactId ?? undefined }))}>
              Open evidence <ArrowUpRight size={13} aria-hidden /></Button>}
            {savedJob && navigation.openQueueJob && <Button type="button" variant="ghost"
              onClick={() => navigation.openQueueJob?.(savedJob.job.id)}>Open job <ArrowUpRight size={13} aria-hidden /></Button>}
          </div>}
        </li>;
      })}</ul>
    </section>}
  </section>;
}
