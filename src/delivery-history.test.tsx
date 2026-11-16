// @vitest-environment jsdom

import { useState } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { Job } from "@/bindings/Job";
import { ProjectTasksWorkspace } from "@/project-tasks";
import { RepositoryDeliveryWorkspace } from "@/repository-delivery";
import type { DeliverySnapshot } from "@/repository-delivery-model";
import { RunOrchestrationPanel } from "@/run-orchestration";
import { exampleCheckout, type WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("reopens a completed task and navigates its original note, selected diff, accepted review, and implementation context", async () => {
  const { projectId, checkoutId } = exampleCheckout;
  const task = { id: "saved-task", title: "Completed release work" };
  const ref = { projectId, taskId: task.id };
  const runId = "saved-workflow";
  const unitId = "saved-unit";
  const sha = "6".repeat(40);
  const receiptId = "commit-receipt";
  const note = { id: "old-note-revision", checkoutId, relativePath: "release.md", sha256: "a".repeat(64) };
  const updated = { ...note, id: "new-note-revision", sha256: "b".repeat(64) };
  const completed = {
    state: "completed", activeWorkflowRunId: runId, hasDirectWork: true,
    checklistResolved: true, pendingRepositoryChanges: false, waitingForCommit: false,
  };
  const job = (node: string, attempt: string, conversationId: string): Job => ({
    id: `${node}-job`, projectId, checkoutId, taskId: task.id, kind: "agent", origin: { type: "pipeline" },
    worktreeOwnerTaskId: task.id, context: null, mode: node === "implement" ? "approved_write" : "read_only",
    pipeline: { pipelineRunId: runId, nodeInstanceId: node, planRevision: "plan", release: null },
    priority: 0, queueState: "queued", queuedRecovery: null, downstreamWait: null,
    latestAttempt: { ordinal: 1, runId: attempt, conversationId, status: "succeeded", cancelRequested: false, previousRunId: null, recovery: null },
  });
  const delivery: DeliverySnapshot = {
    run: { id: runId, planRevision: "plan" }, hostOutputs: { work: "work", prepare: "worktree" },
    jobs: [
      { nodeId: "implement", unitId, inputs: { plan: "plan", worktree: "worktree", release: "release" },
        job: job("implement", "old-writer", "writer-chat") },
      { nodeId: "review-candidate", unitId, inputs: { candidate: "accepted-candidate" },
        job: job("review-candidate", "old-review", "review-chat") },
    ],
    artifacts: [
      { id: "context", nodeInstanceId: "context", port: "context", unitId: null, value: { kind: "markdown", evidence: note } },
      { id: "plan", nodeInstanceId: "plan", port: "plan", unitId: null, value: { kind: "plan", evidence: { run_id: "plan-attempt", event_sequence: 1 } } },
      { id: "work", nodeInstanceId: "work", port: "task", unitId: null, value: { kind: "task", evidence: ref } },
      { id: "approval", nodeInstanceId: "approve", port: "decision", unitId: null, value: { kind: "approval", evidence: { planRevision: "plan" } } },
      { id: "release", nodeInstanceId: "prepare", port: "release", unitId: null, value: { kind: "release", evidence: { decisionArtifactId: "approval", release: { id: "release", revoked: false } } } },
      { id: "worktree", nodeInstanceId: "prepare", port: "worktree", unitId: null, value: { kind: "worktree", evidence: { checkout_id: checkoutId, task: ref } } },
      { id: "accepted-review", nodeInstanceId: "review-candidate", port: "review", unitId, value: { kind: "review", evidence: { run_id: "old-review" } } },
      { id: "unrelated-review", nodeInstanceId: "review-candidate", port: "review", unitId, value: { kind: "review", evidence: { run_id: "different-review" } } },
      { id: "approval-artifact", nodeInstanceId: "accept", port: "acceptance", unitId, value: { kind: "acceptance", evidence: {
        evidence: { task: ref, id: "accepted-receipt" }, reviewArtifactId: "accepted-review", completionCriteria: ["Publish the changes"],
      } } },
      { id: "alternate-approval-artifact", nodeInstanceId: "accept", port: "acceptance", unitId, value: { kind: "acceptance", evidence: {
        evidence: { task: ref, id: "accepted-receipt" }, reviewArtifactId: "unrelated-review", completionCriteria: ["Publish the changes"],
      } } },
    ],
  };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "project_work_hierarchy": return { epics: [], work: [{ task, placement: { kind: "task", epicId: null } }] };
      case "project_task_delivery_state": return completed;
      case "project_task_checklist": return null;
      case "project_task_deliveries": return [{ id: "delivery", recordedAt: "2026-11-16T10:00:00Z", evidence: {
        kind: "verified_commit", workflowRunId: runId, completionSha: sha, unitEvidenceIds: [receiptId],
      } }];
      case "project_task_runs": case "project_task_assignments": case "conversation_models": return [];
      case "list_repository_markdown_revisions": return [updated, note];
      case "list_repository_deliveries": return [delivery.run];
      case "inspect_repository_delivery":
        expect(args).toEqual({ projectId, runId });
        return delivery;
      case "repository_delivery_units": return [{ checkoutId, taskId: task.id, commitEvidenceId: receiptId,
        unit: { id: unitId, ordinal: 1, purpose: "Publish the unit", completionCriteria: ["Publish the changes"] } }];
      case "verified_delivery_commit": return { sha, branch: "codex/historical", captureRunId: "old-writer",
        acceptanceId: "accepted-receipt", acceptanceArtifactId: "approval-artifact", purpose: "Publish the unit",
        verifiedAt: "2026-11-16T10:00:00Z", pendingChangesRetained: false };
      case "verified_delivery_patch":
        expect(args).toEqual({ ...ref, evidenceId: receiptId });
        return { patchBase64: btoa("diff --git a/selected.txt b/selected.txt\n+exact accepted output") };
      case "read_repository_markdown":
        expect(args).toEqual({ checkoutId, revisionId: note.id });
        return { revision: note, markdown: "Original frozen note content" };
      case "run_orchestration":
        expect(args).toEqual({ projectId, runId: "old-writer", conversationId: "writer-chat" });
        return {
          job: null, route: null, stagePin: null, policy: null, goal: null, unit: null, document: null,
          taskDescriptions: { task: { id: "old-description-id", revision: 1, markdown: "Original frozen description" } },
          context: { role: "implementer", policyRevision: "old-policy", availableBytes: 100, usedBytes: 50, decisions: [{
            title: "Original decision", reason: "explicit_link", selectionOrder: 0, encodedBytes: 50,
            revision: { source: "record", version: { revisionId: "old-decision-id" } },
          }] },
        };
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const visited = vi.fn();
  function ReopenedWorkbench() {
    const [workflow, setWorkflow] = useState<string>();
    const [inspection, setInspection] = useState<{ runId: string; conversationId?: string }>();
    const props: WorkspaceProps = { checkout: exampleCheckout, simulation: null, startSimulation: vi.fn(), openChanges: vi.fn(),
      openWorkspace: (name, target) => { if (name === "pipelines") setWorkflow(target); },
      openActivity: (runId, conversationId) => { visited(runId, conversationId); setInspection({ runId, conversationId }); } };
    return <>
      {workflow ? <RepositoryDeliveryWorkspace {...props} target={workflow} /> : <ProjectTasksWorkspace {...props} target={task.id} />}
      {inspection?.runId === "old-writer" && <RunOrchestrationPanel scope={{ projectId, ...inspection }} live={false} />}
    </>;
  }
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><ReopenedWorkbench /></QueryClientProvider>);
  const user = userEvent.setup();
  await screen.findByText("Completed");
  await user.click(await screen.findByText(/^Verified delivery ·/));
  expect(await screen.findByText("Publish the unit · codex/historical")).toBeTruthy();
  expect(screen.getAllByText(sha).length).toBeGreaterThan(0);
  await user.click(screen.getByRole("button", { name: "Open delivery workflow" }));
  await user.click(await screen.findByText("Saved Markdown · release.md"));
  expect(await screen.findByText("Original frozen note content")).toBeTruthy();
  await user.click(await screen.findByText("Accepted diff"));
  expect(await screen.findByText(/exact accepted output/)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Open accepted candidate review" }));
  expect(visited).toHaveBeenLastCalledWith("old-review", "review-chat");
  await user.click(screen.getByRole("button", { name: "Open implementation attempt" }));
  expect(visited).toHaveBeenLastCalledWith("old-writer", "writer-chat");
  await user.click(screen.getByText("Orchestration and worktree"));
  await user.click(await screen.findByText("Context selection reasons"));
  expect(screen.getByText("old-decision-id")).toBeTruthy();
  await user.click(screen.getByText("Saved work descriptions"));
  expect(screen.getByText("Original frozen description")).toBeTruthy();
  expect(screen.getByText("old-description-id")).toBeTruthy();
});
