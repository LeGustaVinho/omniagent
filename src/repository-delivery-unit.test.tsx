// @vitest-environment jsdom

import { useState } from "react";
import { QueryClient, QueryClientProvider, useMutation } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { Job } from "@/bindings/Job";
import type { DeliveryArtifact, DeliverySnapshot, DeliveryUnit } from "@/repository-delivery-model";
import { RepositoryDeliveryUnit } from "@/repository-delivery-unit";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

it("checks for a commit after accepting the diff and verifies the full SHA under a manual Git policy", async () => {
  const scope = { projectId: "project", runId: "workflow" };
  const task = { projectId: scope.projectId, taskId: "task" };
  const unitId = "unit";
  const checkoutId = "managed-checkout";
  const criteria = ["Confirm the API contract", "Cover invalid requests"];
  const implementationInputs = { plan: "plan-artifact", worktree: "worktree-artifact", release: "release-artifact" };
  const unit: DeliveryUnit = {
    checkoutId, taskId: task.taskId, commitEvidenceId: null,
    unit: { id: unitId, ordinal: 1, purpose: "Deliver the API", completionCriteria: criteria },
  };
  const candidate = {
    id: "candidate-artifact", nodeInstanceId: "capture", port: "candidate", unitId,
    value: { kind: "candidate", evidence: {
      id: "selected-candidate", tree: "candidate-tree", target: { runId: "implementation-attempt", unitId },
    } },
  } satisfies DeliveryArtifact;
  const review = {
    id: "review-artifact", nodeInstanceId: "review-candidate", port: "review", unitId,
    value: { kind: "review", evidence: { run_id: "review-attempt" } },
  } satisfies DeliveryArtifact;
  const checks = {
    id: "checks-artifact", nodeInstanceId: "verify-candidate", port: "checks", unitId,
    value: { kind: "checks", evidence: { task, id: "verification" } },
  } satisfies DeliveryArtifact;
  const acceptance = {
    id: "acceptance-artifact", nodeInstanceId: "accept", port: "acceptance", unitId,
    value: { kind: "acceptance", evidence: {
      evidence: { task, id: "acceptance-evidence" }, reviewArtifactId: review.id, completionCriteria: criteria,
    } },
  } satisfies DeliveryArtifact;
  const completedJob = (nodeId: string, runId: string, mode: Job["mode"]): Job => ({
    id: `${nodeId}-job`, projectId: scope.projectId, checkoutId, taskId: task.taskId,
    kind: "agent", origin: { type: "pipeline" }, worktreeOwnerTaskId: task.taskId, context: null, mode,
    pipeline: { pipelineRunId: scope.runId, nodeInstanceId: nodeId, planRevision: implementationInputs.plan, release: null },
    priority: 0, queueState: "queued", queuedRecovery: null, downstreamWait: null,
    latestAttempt: { ordinal: 1, runId, conversationId: null, status: "succeeded", cancelRequested: false, previousRunId: null, recovery: null },
  });
  const delivery: DeliverySnapshot = {
    run: { id: scope.runId, planRevision: implementationInputs.plan },
    artifacts: [candidate, review, checks],
    jobs: [
      { nodeId: "implement", inputs: implementationInputs, unitId, job: completedJob("implement", "implementation-attempt", "approved_write") },
      { nodeId: "review-candidate", inputs: { candidate: candidate.id }, unitId, job: completedJob("review-candidate", "review-attempt", "read_only") },
    ],
  };
  const sha = "1234567890abcdef1234567890abcdef12345678";
  let savedArtifacts = delivery.artifacts;
  let commitEvidenceId: string | null = null;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "attempt_worktree_evidence": return { kind: "saved", artifact: { revision: "capture", candidate: { changes: null } } };
      case "current_selected_candidate": return { id: candidate.value.evidence.id };
      case "inspect_candidate_verification": return {
        id: checks.value.evidence.id, candidateId: candidate.value.evidence.id,
        startedAt: "2026-11-16T10:00:00Z", finishedAt: "2026-11-16T10:01:00Z",
        gates: [{ gate: { name: "API checks", required: true }, outcome: { status: "passed" } }],
      };
      case "inspect_candidate_acceptance": return {
        candidate: { id: candidate.value.evidence.id }, verification: { id: checks.value.evidence.id },
      };
      case "delivery_git_policy": return "user_commits_and_merges";
      case "execute_repository_delivery_unit": {
        const input = (args as { input: { nodeId: string; request: { externalSha?: string | null } } }).input;
        if (input.nodeId === "accept") {
          savedArtifacts = [...delivery.artifacts, acceptance];
          return { status: "artifact", artifact: acceptance };
        }
        if (input.nodeId !== "commit") throw new Error(`Unexpected node ${input.nodeId}`);
        if (input.request.externalSha === null) return { status: "waiting_for_commit" };
        commitEvidenceId = "verified-commit-evidence";
        return { status: "artifact", artifact: { id: "commit-artifact" } };
      }
      case "verified_delivery_commit": return {
        sha, branch: "codex/task-fixture", verifiedAt: "2026-11-16T10:02:00Z",
        captureRunId: "implementation-attempt", pendingChangesRetained: false,
      };
      default: throw new Error(`Unexpected command ${command}`);
    }
  });

  function Fixture() {
    const [snapshot, setSnapshot] = useState({ artifacts: delivery.artifacts, commitEvidenceId: unit.commitEvidenceId });
    const action = useMutation({
      mutationFn: (operation: () => Promise<unknown>) => operation(),
      onSuccess: () => setSnapshot({ artifacts: savedArtifacts, commitEvidenceId }),
    });
    return <RepositoryDeliveryUnit
      scope={scope} unit={{ ...unit, commitEvidenceId: snapshot.commitEvidenceId }}
      delivery={{ ...delivery, artifacts: snapshot.artifacts }} implementationInputs={implementationInputs}
      actions={{ execute: (operation) => action.mutate(operation), busy: action.isPending }}
      openActivity={() => {}} active={!snapshot.commitEvidenceId}
    />;
  }

  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><Fixture /></QueryClientProvider>);
  const user = userEvent.setup();
  const accept = await screen.findByRole<HTMLButtonElement>("button", { name: "Accept diff" });
  expect(accept.disabled).toBe(true);
  await user.click(screen.getByRole("checkbox", { name: criteria[0] }));
  expect(accept.disabled).toBe(true);
  await user.click(screen.getByRole("checkbox", { name: criteria[1] }));
  expect(accept.disabled).toBe(false);
  await user.click(accept);

  const input = await screen.findByRole("textbox", { name: "Full commit SHA" });
  expect(screen.getByText("In review · Waiting for commit")).toBeTruthy();
  const verify = screen.getByRole<HTMLButtonElement>("button", { name: "Verify commit" });
  expect(verify.disabled).toBe(true);
  await user.type(input, sha.slice(0, 7));
  expect(verify.disabled).toBe(true);
  await user.type(input, sha.slice(7));
  expect(verify.disabled).toBe(false);
  await user.click(verify);

  expect(await screen.findByText(sha)).toBeTruthy();
  const executions = vi.mocked(invoke).mock.calls.filter(([command]) => command === "execute_repository_delivery_unit");
  const target = { ...scope, taskId: task.taskId, unitId };
  const commit = { ...target, nodeId: "commit", inputs: { candidate: candidate.id, acceptance: acceptance.id } };
  expect(executions.map(([, args]) => args)).toEqual([
    { input: { ...target, nodeId: "accept", inputs: { candidate: candidate.id, checks: checks.id, review: review.id },
      request: { action: "accept_candidate", completionCriteria: criteria } } },
    { input: { ...commit, request: { action: "verify_commit", externalSha: null } } },
    { input: { ...commit, request: { action: "verify_commit", externalSha: sha } } },
  ]);
});
