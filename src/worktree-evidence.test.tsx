// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { WorktreeEvidence } from "@/worktree-evidence";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

it("keeps unavailable attempt evidence separate from an available live checkout", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "attempt_worktree_evidence") {
      return { kind: "unavailable", reason: "No candidate was saved for this attempt." };
    }
    return { worktree: { branch: "codex/task-fixture" }, head: "live-head", status: ["? later.txt"] };
  });
  const client = new QueryClient();
  render(<QueryClientProvider client={client}>
    <WorktreeEvidence projectId="project" runId="first-run" ownerTaskId="task" />
  </QueryClientProvider>);

  await userEvent.click(screen.getByText("Saved candidate for this attempt"));
  await screen.findByText("No candidate was saved for this attempt.");
  expect(invoke).toHaveBeenCalledWith("attempt_worktree_evidence", { projectId: "project", runId: "first-run" });
  expect(invoke).toHaveBeenCalledTimes(1);

  await userEvent.click(screen.getByText("Live checkout status"));
  await screen.findByText("? later.txt");
  expect(invoke).toHaveBeenCalledWith("live_managed_worktree", { projectId: "project", ownerTaskId: "task" });
  expect(screen.getByText("No candidate was saved for this attempt.")).toBeDefined();
});
