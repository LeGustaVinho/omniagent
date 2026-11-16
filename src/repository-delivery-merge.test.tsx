// @vitest-environment jsdom

import { invoke } from "@tauri-apps/api/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { RepositoryDeliveryMerge } from "@/repository-delivery-merge";
import type { DeliveryMergePreview, DestinationDriftOutcome, VerifiedMerge } from "@/delivery-merge-model";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

const sha = (digit: string) => digit.repeat(40);
const scope = { projectId: "project", runId: "delivery" };
const unit = { taskId: "task", checkoutId: "writer", commitEvidenceId: "source-receipt",
  unit: { id: "logical-unit", ordinal: 2, purpose: "Complete the delivery", completionCriteria: [] } };
const gitArtifactId = "source-git-artifact";
const original: DeliveryMergePreview = {
  sourceBranch: "codex/task", sourceSha: sha("a"), destinationBranch: "integration", expectedDestinationSha: sha("b"),
};
const preview = { id: "drift-preview", sourceSha: original.sourceSha, destinationBranch: original.destinationBranch,
  expectedDestinationSha: original.expectedDestinationSha, destinationSha: sha("c"), tree: sha("d"), changedPaths: ["src/delivered.rs"] };
const verification = { id: "pinned-checks", gates: [{ gate: { name: "Windows tests", required: true }, outcome: { status: "passed" } }] };
const reviewed: DestinationDriftOutcome = { status: "requires_acceptance", preview, verification };

function runPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const execute = (operation: () => Promise<unknown>) => {
    void operation().then(() => client.invalidateQueries({ queryKey: ["delivery-merge"] }));
  };
  render(<QueryClientProvider client={client}><RepositoryDeliveryMerge scope={scope} unit={unit} gitArtifactId={gitArtifactId}
    actions={{ execute, busy: false }} automaticOutcome={undefined} /></QueryClientProvider>);
}

function mergeRequest(expected: { perform: boolean; externalSha: string | null; driftAcceptanceId: string | null; prepareDrift?: boolean }) {
  return { input: { ...scope, taskId: unit.taskId, unitId: unit.unit.id, nodeId: "merge", inputs: { git: gitArtifactId },
    request: { action: "verify_merge", expectedSourceBranch: original.sourceBranch, expectedSourceSha: original.sourceSha,
      destinationBranch: original.destinationBranch, expectedDestinationSha: original.expectedDestinationSha, prepareDrift: false, ...expected } } };
}

it("pauses for explicit review and uses the checked update with the originally pinned refs", async () => {
  let receipt: VerifiedMerge | null = null;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "verified_delivery_merge_for_commit": return receipt;
      case "delivery_merge_preview": return original;
      case "delivery_git_policy": return "automatic_commit_user_merge";
      case "delivery_destination_drift_patch": return { patchBase64: btoa("+updated content at the pinned destination"), truncated: false };
      case "accept_delivery_destination_drift": return { ...reviewed, status: "accepted", acceptanceId: "renewed-acceptance" };
      case "execute_repository_delivery_unit": {
        const request = (args as { input: { request: { driftAcceptanceId: string | null; prepareDrift: boolean } } }).input.request;
        if (request.driftAcceptanceId === "renewed-acceptance") {
          receipt = { id: "merge-receipt", projectId: scope.projectId, workflowRunId: scope.runId, taskId: unit.taskId, unitId: unit.unit.id,
            commitEvidenceId: unit.commitEvidenceId, commitEvidenceIds: [unit.commitEvidenceId], sourceBranch: original.sourceBranch,
            sourceSha: original.sourceSha, destinationBranch: original.destinationBranch, previousDestinationSha: preview.destinationSha,
            integrationSha: sha("e"), verifiedAt: "2026-11-16T00:00:00Z", source: "manual" };
          return { status: "artifact", artifact: { id: "integration-artifact" } };
        }
        return { status: "destination_drift", recovery: request.prepareDrift ? reviewed : {
          status: "paused", expectedDestinationSha: original.expectedDestinationSha, actualSha: preview.destinationSha,
        } };
      }
      default: throw new Error(`Unexpected IPC ${command}`);
    }
  });
  runPanel();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Merge approved delivery" }));
  expect(invoke).toHaveBeenCalledWith("execute_repository_delivery_unit", mergeRequest({ perform: true, externalSha: null, driftAcceptanceId: null }));
  const prepare = await screen.findByRole("button", { name: "Prepare updated candidate" });
  expect(screen.queryByRole("button", { name: "Accept updated candidate" })).toBeNull();
  await user.click(prepare);
  expect(invoke).toHaveBeenCalledWith("execute_repository_delivery_unit",
    mergeRequest({ perform: false, externalSha: null, driftAcceptanceId: null, prepareDrift: true }));
  expect(await screen.findByText("src/delivered.rs")).toBeTruthy();
  expect(screen.getByText("Windows tests · Required · passed")).toBeTruthy();
  expect(screen.getByText(preview.tree)).toBeTruthy();
  await user.click(screen.getByText("Updated candidate diff"));
  expect(await screen.findByText("+updated content at the pinned destination")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Accept updated candidate" }));
  expect(invoke).toHaveBeenCalledWith("accept_delivery_destination_drift", {
    projectId: scope.projectId, taskId: unit.taskId, workflowRunId: scope.runId, previewId: preview.id, verificationId: verification.id,
  });
  await user.click(await screen.findByRole("button", { name: "Merge updated candidate" }));
  expect(invoke).toHaveBeenCalledWith("execute_repository_delivery_unit",
    mergeRequest({ perform: true, externalSha: null, driftAcceptanceId: "renewed-acceptance" }));
  expect(await screen.findByText(sha("e"))).toBeTruthy();
  expect(screen.getByRole("status").textContent).toBe("Verified merge");
});

it("verifies external integration with the same pinned evidence and displays a moved destination", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "verified_delivery_merge_for_commit": return null;
      case "delivery_merge_preview": return original;
      case "delivery_git_policy": return "user_commits_and_merges";
      case "execute_repository_delivery_unit": return { status: "destination_moved", actualSha: preview.destinationSha };
      default: throw new Error(`Unexpected IPC ${command}`);
    }
  });
  runPanel();
  const user = userEvent.setup();
  const button = await screen.findByRole<HTMLButtonElement>("button", { name: "Verify external merge" });
  expect(await screen.findByRole("button", { name: "Review destination" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Merge approved delivery" })).toBeNull();
  expect(button.disabled).toBe(true);
  await user.type(screen.getByRole("textbox", { name: "External integration SHA" }), sha("f"));
  await user.click(button);
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain(preview.destinationSha));
  expect(invoke).toHaveBeenCalledWith("execute_repository_delivery_unit",
    mergeRequest({ perform: false, externalSha: sha("f"), driftAcceptanceId: null }));
});
