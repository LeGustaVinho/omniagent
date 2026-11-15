// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { JobInspection } from "@/bindings/JobInspection";
import QueuePanel from "@/queue-panel";
import type { WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const projectId = "project-1";
const checkoutId = "checkout-1";
const checkout = { projectId, checkoutId, repositoryRoot: "C:/repository", checkoutRoot: "C:/repository" };

const queuedJob: JobInspection = {
  job: {
    id: "job-2", projectId, checkoutId, taskId: null, origin: { type: "operator" },
    context: { conversationId: "conversation-2", contextId: "context-2" }, mode: "read_only",
    priority: 10, queueState: "queued", latestAttempt: null,
  },
  blockers: [
    { type: "dependency", jobId: "job-1", status: "running", queueState: "queued" },
    { type: "capacity", active: 2, limit: 2 },
  ],
};

const runningJob: JobInspection = {
  job: {
    ...queuedJob.job, id: "job-1", priority: 0,
    origin: { type: "agent", runId: "origin-run", requestId: "request-1" },
    context: { conversationId: "conversation-1", contextId: "context-1" },
    latestAttempt: { runId: "run-1", ordinal: 2, status: "running", cancelRequested: false },
  },
  blockers: [],
};

function renderQueue(overrides: Partial<WorkspaceProps> & { active?: boolean } = {}) {
  const openActivity = vi.fn();
  const openWorkspace = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <QueuePanel checkout={checkout} simulation={null} startSimulation={() => {}} openWorkspace={openWorkspace}
        openActivity={openActivity} openChanges={() => {}} {...overrides} />
    </QueryClientProvider>,
  );
  return { openActivity, openWorkspace };
}

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

describe("project queue", () => {
  it("reads the selected project and exposes the scheduler blockers and linked attempt", async () => {
    vi.mocked(invoke).mockResolvedValue({ settings: { paused: false, capacity: 2 }, active: 2, jobs: [queuedJob, runningJob] });
    const user = userEvent.setup();
    const { openActivity, openWorkspace } = renderQueue();
    await user.click(await screen.findByRole("button", { name: "Conversation turn" }));
    expect(invoke).toHaveBeenCalledWith("project_job_queue", { projectId });
    expect(screen.getByText("2 / 2 slots")).toBeDefined();

    const blockers = screen.getByRole("region", { name: "Admission blockers" });
    expect(within(blockers).getByText("Waiting for an execution slot (2 of 2 in use).")).toBeDefined();
    await user.click(screen.getByRole("button", { name: "Inspect dependency" }));
    expect(within(screen.getByRole("complementary", { name: "Selected job" })).getByText("#2")).toBeDefined();
    await user.click(screen.getByRole("button", { name: "Inspect attempt" }));
    expect(openActivity).toHaveBeenCalledWith("run-1", "conversation-1");
    await user.click(screen.getByRole("button", { name: "Open conversation" }));
    expect(openWorkspace).toHaveBeenCalledWith("conversations", "conversation-1");
  });

  it("does not request real queue data when the pane is hidden or in simulation", async () => {
    const user = userEvent.setup();
    renderQueue({ active: false });
    expect(invoke).not.toHaveBeenCalled();
    cleanup();

    renderQueue({ simulation: { scenario: "delivery", epoch: 1, delivery: "queued", advanceDelivery: () => {} } });
    await user.click(screen.getByRole("button", { name: "Pause queue" }));
    expect(screen.getByRole("button", { name: "Resume queue" })).toBeDefined();
    expect(invoke).not.toHaveBeenCalled();
  });
});
