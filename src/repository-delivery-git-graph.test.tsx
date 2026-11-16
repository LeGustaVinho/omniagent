// @vitest-environment jsdom

import type { ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { RepositoryDeliveryGitGraph } from "@/repository-delivery-git-graph";
import { deliveryGitControlId, type DeliverySnapshot, type DeliveryUnit } from "@/repository-delivery-model";
import type { VerifiedMerge } from "@/delivery-merge-model";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@xyflow/react", () => ({
  Position: { Left: "left", Right: "right" },
  ReactFlow: ({ nodes, edges, onNodeClick }: {
    nodes: { id: string; data: { label: ReactNode }; ariaLabel: string }[];
    edges: { id: string; source: string; target: string }[];
    onNodeClick: (event: null, node: { id: string }) => void;
  }) => <div aria-label="Pinned nodes and connections">
    {nodes.map((node) => <button key={node.id} aria-label={node.ariaLabel} onClick={() => onNodeClick(null, node)}>
      {node.data.label}
    </button>)}
    {edges.map((edge) => <span key={edge.id}>{edge.source} → {edge.target}</span>)}
  </div>,
}));

afterEach(() => { cleanup(); vi.resetAllMocks(); });

const scope = { projectId: "project", runId: "saved-workflow" };
const taskId = "task";
const commitNode = "publish-source";
const mergeNode = "integrate-source";
const delivery: DeliverySnapshot = {
  run: { id: scope.runId, planRevision: "plan" },
  artifacts: [], jobs: [],
  definition: {
    nodes: [
      { instance: { id: commitNode, kind: "verify_commit" }, definition: { title: "Commit" } },
      { instance: { id: mergeNode, kind: "verify_merge" }, definition: { title: "Merge" } },
    ],
    wires: [{ from: { node: commitNode, port: "git" }, to: { node: mergeNode, port: "git" } }],
  },
};
const units: DeliveryUnit[] = [
  { checkoutId: "approved-worktree", taskId, commitEvidenceId: null, unit: { id: "first", ordinal: 1, purpose: "Implement API", completionCriteria: [] } },
  { checkoutId: "approved-worktree", taskId, commitEvidenceId: null, unit: { id: "second", ordinal: 2, purpose: "Update tests", completionCriteria: [] } },
];
const sourceBranch = "codex/approved-delivery";
const firstSha = "a".repeat(40);
const finalSha = "b".repeat(40);
const integrationSha = "c".repeat(40);
const merged: VerifiedMerge = {
  id: "merge-receipt", projectId: scope.projectId, taskId, workflowRunId: scope.runId,
  unitId: "second", commitEvidenceId: "commit-second", commitEvidenceIds: ["commit-first", "commit-second"],
  sourceBranch, sourceSha: finalSha, destinationBranch: "integration",
  previousDestinationSha: "d".repeat(40), integrationSha, verifiedAt: "2026-11-16T10:30:00Z", source: "operator",
};

it("uses the pinned Git nodes and sends manual actions to the scoped unit controls while evidence is missing", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "delivery_git_policy") return "user_commits_and_merges";
    throw new Error(`Unexpected command ${command}`);
  });
  const openWorkspace = vi.fn();
  render(<QueryClientProvider client={new QueryClient()}>
    <RepositoryDeliveryGitGraph scope={scope} delivery={delivery} units={units} taskId={taskId}
      active waitingForCommit openWorkspace={openWorkspace} />
    <section id={deliveryGitControlId(scope.runId, commitNode, "first")} tabIndex={-1} aria-label="First unit controls" />
  </QueryClientProvider>);
  expect(await screen.findByText("Project Git mode · User commits and merges")).toBeTruthy();
  expect(screen.getByText("publish-source → integrate-source")).toBeTruthy();
  expect(screen.getByText("Waiting for a commit SHA from the approved branch.")).toBeTruthy();
  expect(screen.getByText("Waiting for the preceding verified unit commit.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Open commit controls" }));
  expect(document.activeElement).toBe(screen.getByRole("region", { name: "First unit controls" }));
  fireEvent.click(screen.getByRole("button", { name: "Merge node" }));
  expect(screen.getByText("Waiting for all verified unit commits.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Open linked work" }));
  expect(openWorkspace).toHaveBeenCalledWith("tasks", taskId);
  expect(invoke).toHaveBeenCalledExactlyOnceWith("delivery_git_policy", { projectId: scope.projectId });
});

it("renders each saved receipt and switches from the real merge wait to verified integration after reload", async () => {
  let integration: VerifiedMerge | null = null;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "delivery_git_policy") return "automatic_commit_user_merge";
    if (command === "verified_delivery_commit") {
      const evidenceId = (args as { evidenceId: string }).evidenceId;
      const first = evidenceId === "commit-first";
      return { id: evidenceId, projectId: scope.projectId, taskId, unitId: first ? "first" : "second",
        branch: sourceBranch, sha: first ? firstSha : finalSha };
    }
    if (command === "verified_delivery_merge_for_commit") return integration;
    throw new Error(`Unexpected command ${command}`);
  });
  const completed = units.map((unit) => ({ ...unit, commitEvidenceId: `commit-${unit.unit.id}` }));
  const mount = () => render(<QueryClientProvider client={new QueryClient()}>
    <RepositoryDeliveryGitGraph scope={scope} delivery={delivery} units={completed} taskId={taskId}
      active waitingForCommit={false} openWorkspace={vi.fn()} />
    <section id={deliveryGitControlId(scope.runId, mergeNode, "second")} tabIndex={-1} aria-label="Final merge controls" />
  </QueryClientProvider>);
  const initial = mount();
  const commitDetails = within(screen.getByRole("region", { name: "Commit node evidence" }));
  expect(await commitDetails.findByText(firstSha)).toBeTruthy();
  expect(await commitDetails.findByText(finalSha)).toBeTruthy();
  expect(commitDetails.getAllByText(new RegExp(sourceBranch))).toHaveLength(2);
  fireEvent.click(screen.getByRole("button", { name: "Inspect Merge" }));
  expect(await screen.findByText("Waiting for the user to start integration.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Open merge controls" }));
  expect(document.activeElement).toBe(screen.getByRole("region", { name: "Final merge controls" }));
  initial.unmount();

  integration = merged;
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Inspect Merge" }));
  const mergeDetails = within(screen.getByRole("region", { name: "Merge node evidence" }));
  expect(await mergeDetails.findByText(integrationSha)).toBeTruthy();
  expect(mergeDetails.getByText(finalSha)).toBeTruthy();
  expect(mergeDetails.getByText(/integration ·/)).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("verified_delivery_merge_for_commit", {
    projectId: scope.projectId, taskId, commitEvidenceId: "commit-second",
  });
});
