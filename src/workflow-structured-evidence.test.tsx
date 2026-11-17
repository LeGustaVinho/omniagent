// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ExecutionArtifact } from "@/workflow-execution-model";
import { WorkflowStructuredEvidence } from "@/workflow-structured-evidence";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function renderEvidence(producer: string, port: string, kind: string, evidence: unknown) {
  const artifact: ExecutionArtifact = {
    id: "evidence", nodeInstanceId: "stage", workflowRunId: "parent", port, unitId: null, value: { kind, evidence },
  };
  const openWorkspace = vi.fn();
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const result = render(<QueryClientProvider client={queryClient}>
    <WorkflowStructuredEvidence artifact={artifact} producer={producer} projectId="selected-project" openWorkspace={openWorkspace} />
  </QueryClientProvider>);
  return { ...result, openWorkspace };
}

it("distinguishes a verified red phase from the expected test failure it recorded", async () => {
  renderEvidence("verify_test_phase", "evidence", "test_phase_evidence", {
    phase: "red", check: "Test release", baseCommit: "base-sha",
    specification: { paths: ["release.test"], tests: [{ id: "release check", reason: "validation required" }] }, result: {
      verdict: "verified", testPatchRevision: "test-only-revision", exitCode: 1,
      report: { status: "completed", tests: [{ id: "release check", status: "failed", reason: "missing validation" }] },
    },
  });
  expect(screen.getByRole("region", { name: "Test phase evidence" })).toBeTruthy();
  expect(screen.getByText("Expected failure reproduced")).toBeTruthy();
  expect(screen.getByText("test-only-revision")).toBeTruthy();
  const user = userEvent.setup();
  await user.click(screen.getByText("Expected failures (1)"));
  expect(screen.getByText("validation required")).toBeTruthy();
  await user.click(screen.getByText("Recorded test results (1)"));
  expect(screen.getByText("missing validation")).toBeTruthy();
  expect(screen.getByText("Failed")).toBeTruthy();
});

it("shows operator findings for a quality gate and ignores unrelated JSON", () => {
  const report = { decision: "blocked", blockers: ["qa.result"], findings: ["Missing build"] };
  const unrelated = renderEvidence("agent", "decision", "json_object", report);
  expect(unrelated.container.firstChild).toBeNull();
  unrelated.unmount();
  renderEvidence("quality_gate", "decision", "json_object", report);
  expect(screen.getByText("Blocked")).toBeTruthy();
  expect(screen.getByText("qa.result")).toBeTruthy();
  expect(screen.getByText("Missing build")).toBeTruthy();
});

it("keeps skipped and failed evidence distinct and ignores malformed entries", () => {
  renderEvidence("collect_evidence", "evidence", "evidence_set", [
    { source: { node: "quality", port: "result" }, state: "completed", artifactId: "verified-check" },
    { source: { node: "qa", port: "result" }, state: "failed" },
    { source: { node: "review", port: "result" }, state: "skipped" },
    { source: null, state: "failed" },
  ]);
  expect(screen.getByText("Collected")).toBeTruthy();
  expect(screen.getByText("Source failed")).toBeTruthy();
  expect(screen.getByText("Skipped")).toBeTruthy();
  expect(screen.getAllByRole("listitem")).toHaveLength(3);
});

it("opens the recorded child workflow only for a task from the current project", async () => {
  const { openWorkspace } = renderEvidence("run_backlog", "deliveries", "json_object", { tasks: [
    { key: "ship", task: { projectId: "selected-project", taskId: "task" }, workflowRunId: "child", integrationSha: "integrated-sha", destinationBranch: "main" },
    { key: "elsewhere", task: { projectId: "other-project", taskId: "other" }, workflowRunId: "outside", integrationSha: "another-sha" },
  ] });
  expect(screen.getByText("integrated-sha")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Open workflow for elsewhere" })).toBeNull();
  await userEvent.setup().click(screen.getByRole("button", { name: "Open workflow for ship" }));
  expect(openWorkspace).toHaveBeenCalledExactlyOnceWith("pipelines", "run:child");
});

it("leaves an unrecognized test verdict in the standard artifact inspector", () => {
  const { container } = renderEvidence("verify_test_phase", "evidence", "test_phase_evidence", {
    phase: "green", check: "Unit tests", result: { verdict: "unknown" },
  });
  expect(container.firstChild).toBeNull();
});

it("shows the pinned integration tip and the exact task merges it contains", async () => {
  renderEvidence("run_backlog", "integrated", "integrated_revision", {
    destinationBranch: "main", sha: "verified-tip", integrationShas: ["task-merge-a", "task-merge-b"],
  });
  expect(screen.getByRole("region", { name: "Integrated revision" })).toBeTruthy();
  expect(screen.getByText("verified-tip")).toBeTruthy();
  await userEvent.setup().click(screen.getByText("Included task merges (2)"));
  expect(screen.getByText("task-merge-b")).toBeTruthy();
});

it("distinguishes a blocked integrated verification from completed check results", () => {
  renderEvidence("verify_integrated_checks", "checks", "integrated_checks_evidence", {
    revision: { destinationBranch: "release", sha: "integrated-sha", integrationShas: ["task-merge"] },
    tree: "checked-tree", verdict: "blocked", reason: "A required check could not start.",
    checks: [{ name: "Unit", outcome: { status: "passed" } },
      { name: "Package", outcome: { status: "could_not_run", reason: "Packager unavailable" } }],
  });
  expect(screen.getByText("Blocked")).toBeTruthy();
  expect(screen.getByText("checked-tree")).toBeTruthy();
  const results = within(screen.getByRole("list", { name: "Check results" }));
  expect(results.getByText("Passed")).toBeTruthy();
  expect(results.getByText("Could not run")).toBeTruthy();
  expect(results.getByText("Packager unavailable")).toBeTruthy();
});

it("shows the GitHub PR and CI artifacts with separate head, base, and tested commits", () => {
  const pull = { number: 42, url: "https://github.com/example/project/pull/42", headSha: "pr-head", baseSha: "pr-base",
    headBranch: "task", baseBranch: "main", open: true, merged: false, draft: false };
  const summary = renderEvidence("open_or_update_pull_request", "pull", "github_pull_request", { pull, requiresHuman: true });
  expect(screen.getByText("GitHub pull request #42")).toBeTruthy();
  expect(screen.getByText("Delivery policy requires human approval.")).toBeTruthy();
  expect(screen.getByText(pull.url)).toBeTruthy();
  summary.unmount();
  renderEvidence("wait_for_ci", "ci", "github_ci", { pull: { pull }, snapshot: {
    pullNumber: 42, headSha: "pr-head", baseSha: "pr-base", testedSha: "merge-test", state: "failed",
    checks: [{ context: "Lint", state: "passed" }, { context: "Tests", state: "failed" }],
    requiredChecks: [{ context: "Tests", appId: null }],
  } });
  expect(screen.getByText("merge-test")).toBeTruthy();
  expect(screen.getByText("pr-head")).toBeTruthy();
  expect(screen.getByText("pr-base")).toBeTruthy();
  expect(within(screen.getByRole("list", { name: "GitHub check results" })).getByText("Failed")).toBeTruthy();
});

it("loads exact candidate check results only when the same-project receipt is expanded", async () => {
  vi.mocked(invoke).mockResolvedValue({
    id: "verification-id",
    gates: [{ gate: { name: "Unit", required: true }, outcome: { status: "passed" } },
      { gate: { name: "Build", required: true }, outcome: null, deferred: true }],
  });
  const foreign = renderEvidence("verify_candidate", "checks", "checks", {
    id: "foreign-verification", task: { projectId: "outside", taskId: "foreign" },
  });
  expect(foreign.container.firstChild).toBeNull();
  foreign.unmount();
  renderEvidence("verify_candidate", "checks", "checks", {
    id: "verification-id", task: { projectId: "selected-project", taskId: "task" },
  });
  expect(invoke).not.toHaveBeenCalled();
  await userEvent.setup().click(screen.getByText("Candidate check results"));
  expect(await screen.findByText("Deferred")).toBeTruthy();
  expect(invoke).toHaveBeenCalledExactlyOnceWith("inspect_candidate_verification", {
    projectId: "selected-project", taskId: "task", verificationId: "verification-id",
  });
});
