// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { GoalVerificationPanel } from "@/goal-verification-panel";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("binds selected candidate evidence to each ordinal and keeps the provider verdict visible", async () => {
  const testEvidence = { kind: "candidate_gate", verificationId: "test-result", gateIndex: 0 };
  const approval = { kind: "candidate_acceptance", acceptanceId: "accepted-result" };
  const choices = [
    { label: "Integration test", candidateId: "result-tree", status: "passed", evidence: testEvidence },
    { label: "Candidate acceptance", candidateId: "result-tree", status: "passed", evidence: approval },
  ];
  const criteria: { ordinal: number; text: string; status: string; evidence: unknown }[] = [
    { ordinal: 1, text: "Run the required test", status: "pending", evidence: null },
    { ordinal: 2, text: "Review the result", status: "pending", evidence: null },
  ];
  let saved = {
    revisionId: "revision", runId: "run", contextId: "result", providerResult: "completed",
    status: "verifying", candidateId: null as string | null, criteria,
  };
  vi.mocked(invoke).mockImplementation(async (command, raw) => {
    if (command === "goal_verification") return saved as never;
    if (command === "list_goal_criterion_evidence") return choices as never;
    if (command === "record_goal_criterion_evidence") {
      const { ordinal, evidence } = raw as { ordinal: number; evidence: unknown };
      const updated = saved.criteria.map((criterion) => criterion.ordinal === ordinal
        ? { ...criterion, status: "passed", evidence } : criterion);
      saved = {
        ...saved, candidateId: "result-tree", criteria: updated,
        status: updated.every(({ status }) => status === "passed") ? "completed" : "verifying",
      };
      return saved as never;
    }
    throw new Error(`Unexpected command ${command}`);
  });
  render(<QueryClientProvider client={new QueryClient()}>
    <GoalVerificationPanel projectId="project" runId="run" />
  </QueryClientProvider>);
  expect(await screen.findByText("Verification in progress")).toBeTruthy();
  expect(screen.getByText("Provider verdict completed")).toBeTruthy();
  const user = userEvent.setup();
  const [first, second] = within(screen.getByRole("list", { name: "Required goal criteria" })).getAllByRole("listitem");
  if (!first || !second) throw new Error("Missing criteria");
  await within(first).findByRole("option", { name: /Integration test/ });
  await user.selectOptions(within(first).getByLabelText("Evidence for criterion 1"), JSON.stringify(testEvidence));
  await user.click(within(first).getByRole("button", { name: "Record evidence" }));
  await within(first).findByText("Passed");
  await user.selectOptions(within(second).getByLabelText("Evidence for criterion 2"), JSON.stringify(approval));
  await user.click(within(second).getByRole("button", { name: "Record evidence" }));
  expect(await screen.findByText("Verified and complete")).toBeTruthy();
  expect(screen.getByText("Provider verdict completed")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("record_goal_criterion_evidence", {
    projectId: "project", runId: "run", ordinal: 1, evidence: testEvidence,
  });
  expect(invoke).toHaveBeenCalledWith("record_goal_criterion_evidence", {
    projectId: "project", runId: "run", ordinal: 2, evidence: approval,
  });
  expect(screen.queryByRole("button", { name: "Record evidence" })).toBeNull();
});
