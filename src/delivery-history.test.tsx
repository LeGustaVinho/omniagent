// @vitest-environment jsdom

import { useState } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ProjectTasksWorkspace } from "@/project-tasks";
import { AttemptDeliveryHistory } from "@/repository-delivery-history";
import type { VerifiedDeliveryUnitHistory } from "@/project-agile-model";
import { RunOrchestrationPanel } from "@/run-orchestration";
import { exampleCheckout, type WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("reopens a completed task and navigates its selected diff, accepted review, and implementation context", async () => {
  const { projectId, checkoutId } = exampleCheckout;
  const task = { id: "saved-task", title: "Completed release work" };
  const ref = { projectId, taskId: task.id };
  const runId = "saved-workflow";
  const unitId = "saved-unit";
  const sha = "6".repeat(40);
  const receiptId = "commit-receipt";
  const completed = {
    state: "completed", activeWorkflowRunId: runId, hasDirectWork: true,
    checklistResolved: true, pendingRepositoryChanges: false, waitingForCommit: false,
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
      case "verified_work_delivery_units": return [{ workflowRunId: runId, taskId: task.id, planRevision: "plan",
        approvalArtifactId: "approval", implementation: { runId: "old-writer", conversationId: "writer-chat" },
        review: { runId: "old-review", conversationId: "review-chat" }, commit: { id: receiptId, projectId, taskId: task.id, unitId,
          sha, branch: "codex/historical", purpose: "Publish the unit", captureRunId: "old-writer" } }];
      case "verified_delivery_merge": return null;
      case "verified_delivery_commit": return { sha, branch: "codex/historical", captureRunId: "old-writer",
        acceptanceId: "accepted-receipt", acceptanceArtifactId: "approval-artifact", purpose: "Publish the unit",
        verifiedAt: "2026-11-16T10:00:00Z", pendingChangesRetained: false };
      case "verified_delivery_patch":
        expect(args).toEqual({ ...ref, evidenceId: receiptId });
        return { patchBase64: btoa("diff --git a/selected.txt b/selected.txt\n+exact accepted output") };
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
    const [inspection, setInspection] = useState<{ runId: string; conversationId?: string }>();
    const props: WorkspaceProps = { checkout: exampleCheckout, simulation: null, startSimulation: vi.fn(), openChanges: vi.fn(),
      openWorkspace: vi.fn(),
      openActivity: (runId, conversationId) => { visited(runId, conversationId); setInspection({ runId, conversationId }); } };
    return <>
      <ProjectTasksWorkspace {...props} target={task.id} />
      {inspection?.runId === "old-writer" && <RunOrchestrationPanel scope={{ projectId, ...inspection }} live={false} />}
    </>;
  }
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><ReopenedWorkbench /></QueryClientProvider>);
  const user = userEvent.setup();
  const detail = within(await screen.findByRole("region", { name: task.title }));
  await waitFor(() => expect(detail.getByRole("status").textContent).toBe("Completed"));
  await user.click(detail.getByRole("button", { name: "Work history" }));
  await user.click(await detail.findByText(/^Verified delivery ·/));
  expect(await screen.findByText("Publish the unit · codex/historical")).toBeTruthy();
  expect(screen.getAllByText(sha).length).toBeGreaterThan(0);
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

it("shows full receipts for the Task, its independent units, and each contributing Sub-Task", async () => {
  const { projectId } = exampleCheckout;
  const parent = { id: "deliver", title: "Ship release" };
  const child = { id: "child", title: "Ship API" };
  const second = { id: "second-child", title: "Ship documentation" };
  const unrelated = { id: "other", title: "Separate work" };
  const recordedAt = "2026-11-16T10:00:00Z";
  const unit = (taskId: string, key: string, purpose: string, sha: string): VerifiedDeliveryUnitHistory => ({
    workflowRunId: `workflow-${key}`, planRevision: `plan-${key}`, approvalArtifactId: `plan-approval-${key}`,
    implementation: { runId: `writer-${key}`, conversationId: `writer-chat-${key}` },
    review: { runId: `review-${key}`, conversationId: `review-chat-${key}` },
    commit: {
      id: `commit-${key}`, projectId, taskId, unitId: `unit-${key}`, branch: `codex/deleted-${key}`, sha,
      purpose, captureRunId: `writer-${key}`, verifiedAt: recordedAt, tree: `tree-${key}`, candidateRevision: `candidate-${key}`,
      acceptanceId: `accept-${key}`, acceptanceArtifactId: `accepted-review-${key}`, verificationId: `verified-${key}`,
      pendingChangesRetained: false,
    },
  });
  const own = unit(parent.id, "parent", "Publish release", "1".repeat(40));
  const draftUnit = unit(parent.id, "unfinished", "Prepare migration separately", "2".repeat(40));
  const childUnit = unit(child.id, "api", "Publish API", "3".repeat(40));
  const secondUnit = unit(second.id, "docs", "Publish docs", "4".repeat(40));
  const other = unit(unrelated.id, "other", "Independent test", "5".repeat(40));
  const units = [own, draftUnit, childUnit, secondUnit, other];
  const completion = (history: VerifiedDeliveryUnitHistory) => ({
    id: `delivery-${history.commit.taskId}`, recordedAt, evidence: {
      kind: "verified_commit", workflowRunId: history.workflowRunId, completionSha: history.commit.sha, unitEvidenceIds: [history.commit.id],
    },
  });
  const merge = {
    id: "merge-receipt", projectId, taskId: parent.id, workflowRunId: own.workflowRunId,
    unitId: own.commit.unitId, commitEvidenceId: own.commit.id, commitEvidenceIds: [own.commit.id], sourceBranch: own.commit.branch,
    sourceSha: own.commit.sha, destinationBranch: "refs/heads/release", previousDestinationSha: "6".repeat(40),
    integrationSha: "7".repeat(40), verifiedAt: recordedAt, source: "external",
  };

  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const input = (args ?? {}) as { projectId?: string; taskId?: string; runId?: string; workflowRunId?: string;
      evidenceId?: string; acceptanceId?: string };
    switch (command) {
      case "project_work_hierarchy":
        return { epics: [], work: [parent, child, second, unrelated].map((task) => ({ task, placement: task === child || task === second
          ? { kind: "subtask", parentTaskId: parent.id } : { kind: "task", epicId: null } })) };
      case "project_task_delivery_state": return { state: "completed", activeWorkflowRunId: null, hasDirectWork: true,
        checklistResolved: true, pendingRepositoryChanges: false, waitingForCommit: false };
      case "project_task_checklist": return null;
      case "project_task_deliveries": {
        const entry = units.find(({ commit }) => commit.taskId === input.taskId);
        return entry ? [completion(entry)] : [];
      }
      case "project_task_assignments": case "conversation_models": case "project_task_goals": case "project_task_runs":
      case "repository_note_catalog": return [];
      case "project_description_editor": return { note: null, related: [], revision: null };
      case "verified_work_delivery_units":
        expect(input.projectId).toBe(projectId);
        return units.filter(({ commit }) => commit.taskId === input.taskId);
      case "verified_delivery_commit":
        return units.find(({ commit }) => commit.id === input.evidenceId && commit.taskId === input.taskId)?.commit ?? null;
      case "inspect_candidate_acceptance": {
        expect(input.projectId).toBe(projectId);
        const entry = units.find(({ commit }) => commit.acceptanceId === input.acceptanceId && commit.taskId === input.taskId);
        if (!entry) throw new Error("Acceptance outside work scope");
        const { commit } = entry;
        return { id: commit.acceptanceId, acceptedAt: recordedAt,
          candidate: { id: commit.candidateRevision, tree: commit.tree,
            target: { projectId, taskId: commit.taskId, unitId: commit.unitId, runId: commit.captureRunId } },
          verification: { id: commit.verificationId, candidateId: commit.candidateRevision, finishedAt: recordedAt,
            gates: [{ gate: { name: "Cargo tests", required: true }, outcome: { status: "passed", stdout: "2 tests passed" } },
              { gate: { name: "Optional review", required: false }, outcome: { status: "could_not_run", reason: "No optional runner" } }] },
        };
      }
      case "verified_delivery_merge":
        return input.taskId === parent.id && input.workflowRunId === own.workflowRunId ? merge : null;
      case "verified_attempt_deliveries":
        expect(input).toEqual({ projectId, runId: own.implementation.runId });
        return [own];
      case "verified_delivery_patch":
        expect(input).toEqual({ projectId, taskId: parent.id, evidenceId: own.commit.id });
        return { patchBase64: btoa("+frozen accepted diff") };
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const navigation = { openActivity: vi.fn(), openWorkspace: vi.fn() };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const app = render(<QueryClientProvider client={client}>
    <ProjectTasksWorkspace checkout={exampleCheckout} target={parent.id} simulation={null} startSimulation={vi.fn()} openChanges={vi.fn()} {...navigation} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  const parentDetail = within(await screen.findByRole("region", { name: parent.title }));
  await user.click(parentDetail.getByRole("button", { name: "Work history" }));
  const history = within(await parentDetail.findByRole("region", { name: "Delivery history" }));
  await user.click(history.getByText(/^Verified delivery ·/));
  const receipt = within(await history.findByRole("region", { name: `Delivery unit ${own.commit.purpose}` }));
  expect(receipt.getByText(own.commit.sha)).toBeTruthy();
  expect(receipt.getByText(own.commit.candidateRevision)).toBeTruthy();
  expect(receipt.getByText(own.approvalArtifactId!)).toBeTruthy();
  expect(receipt.getByText(own.commit.acceptanceId)).toBeTruthy();
  expect(await receipt.findByText("Cargo tests · Required · Passed")).toBeTruthy();
  expect(await receipt.findByText("Optional review · Optional · Could not run")).toBeTruthy();
  await user.click(receipt.getByText("Accepted diff"));
  expect(await receipt.findByText("+frozen accepted diff")).toBeTruthy();
  const integration = within(await history.findByRole("region", { name: "Recorded integration" }));
  expect(integration.getByText(merge.destinationBranch)).toBeTruthy();
  expect(integration.getByText(merge.integrationSha)).toBeTruthy();
  expect(history.getAllByText(own.commit.sha)).toHaveLength(2);

  const independent = within(await parentDetail.findByRole("region", { name: "Other committed units" }));
  await user.click(independent.getByText(draftUnit.commit.purpose, { selector: "summary" }));
  expect(await independent.findByText(draftUnit.commit.sha)).toBeTruthy();
  const contributors = within(await parentDetail.findByRole("region", { name: "Contributing Sub-Task deliveries" }));
  expect(contributors.getAllByRole("button")).toHaveLength(2);
  expect(contributors.queryByRole("button", { name: /Separate work/ })).toBeNull();
  await user.click(contributors.getByRole("button", { name: `Open Sub-Task delivery for ${child.title}` }));
  const childDetail = within(await screen.findByRole("region", { name: child.title }));
  expect(childDetail.getByRole("button", { name: "Work history", pressed: true })).toBeTruthy();
  const childHistory = within(await childDetail.findByRole("region", { name: "Delivery history" }));
  expect(childHistory.getByText(/^Verified delivery ·/).closest("details")?.open).toBe(true);
  expect(await childHistory.findByText(childUnit.commit.candidateRevision)).toBeTruthy();
  expect(childHistory.queryByText(merge.integrationSha)).toBeNull();
  expect(childHistory.queryByText(own.commit.sha)).toBeNull();
  app.unmount();

  render(<QueryClientProvider client={client}>
    <AttemptDeliveryHistory projectId={projectId} runId={own.implementation.runId} navigation={navigation} />
  </QueryClientProvider>);
  const attempt = within(await screen.findByRole("region", { name: "Delivery evidence for this attempt" }));
  expect(await attempt.findByText(own.commit.candidateRevision)).toBeTruthy();
  expect(await attempt.findByText(merge.integrationSha)).toBeTruthy();
  expect(attempt.queryByRole("button", { name: "Open implementation attempt" })).toBeNull();
  await user.click(attempt.getByRole("button", { name: "Open accepted candidate review" }));
  expect(navigation.openActivity).toHaveBeenLastCalledWith(own.review?.runId, own.review?.conversationId);
});
