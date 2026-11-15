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
    id: "job-2", projectId, checkoutId, taskId: null, worktreeOwnerTaskId: null, origin: { type: "operator" },
    context: { conversationId: "conversation-2", contextId: "context-2" }, mode: "read_only",
    priority: 10, queueState: "queued", latestAttempt: null, queuedRecovery: null, pipeline: null, kind: "agent",
  },
  blockers: [
    { type: "dependency", jobId: "job-1", status: "running", queueState: "queued" },
    { type: "capacity", active: 2, limit: 2 },
  ],
  recoveryActions: [],
};

const runningJob: JobInspection = {
  job: {
    ...queuedJob.job, id: "job-1", priority: 0,
    origin: { type: "agent", runId: "origin-run", requestId: "request-1" },
    context: { conversationId: "conversation-1", contextId: "context-1" },
    latestAttempt: { runId: "run-1", conversationId: "attempt-conversation", ordinal: 2, status: "running", cancelRequested: false, previousRunId: null, recovery: null },
  },
  blockers: [],
  recoveryActions: [],
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
    expect(openActivity).toHaveBeenCalledWith("run-1", "attempt-conversation");
    await user.click(screen.getByRole("button", { name: "Open conversation" }));
    expect(openWorkspace).toHaveBeenCalledWith("conversations", "attempt-conversation");
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

  it("shows a queued recovery in Waiting while retaining its previous attempt", async () => {
    const recovery: JobInspection = {
      ...runningJob,
      job: {
        ...runningJob.job,
        queuedRecovery: "resume",
        latestAttempt: {
          runId: "interrupted-run", conversationId: "attempt-conversation", ordinal: 1,
          status: "interrupted", cancelRequested: false, previousRunId: null, recovery: null,
        },
      },
    };
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === "job_failure" ? null : { settings: { paused: false, capacity: 2 }, active: 0, jobs: [recovery] });
    const user = userEvent.setup();
    const { openActivity } = renderQueue();
    await user.click(screen.getByRole("button", { name: "Waiting" }));
    await user.click(await screen.findByRole("button", { name: "Agent job" }));
    expect(screen.getByText("Checkpoint queued for resumption.")).toBeDefined();
    await user.click(screen.getByRole("button", { name: "Inspect attempt" }));
    expect(openActivity).toHaveBeenCalledWith("interrupted-run", "attempt-conversation");
  });

  it("pauses and resumes the scheduler and changes the selected pending job through the runtime", async () => {
    let paused = false;
    let job = queuedJob;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "set_job_queue_paused" && args && "paused" in args) paused = args.paused === true;
      if (command === "set_pending_job_state" && args && "state" in args) {
        job = { ...job, job: { ...job.job, queueState: args.state as "queued" | "paused" | "cancelled" } };
      }
      if (command === "project_job_queue") {
        return { settings: { paused, capacity: 2 }, active: 2, jobs: [job] };
      }
    });
    const user = userEvent.setup();
    renderQueue();
    await user.click(await screen.findByRole("button", { name: "Pause queue" }));
    expect(invoke).toHaveBeenCalledWith("set_job_queue_paused", { paused: true });
    await user.click(await screen.findByRole("button", { name: "Resume queue" }));
    expect(invoke).toHaveBeenCalledWith("set_job_queue_paused", { paused: false });
    await user.click(screen.getByRole("button", { name: "Conversation turn" }));
    await user.click(await screen.findByRole("button", { name: "Pause job" }));
    expect(invoke).toHaveBeenCalledWith("set_pending_job_state", { projectId, jobId: queuedJob.job.id, state: "paused" });
    await user.click(await screen.findByRole("button", { name: "Resume job" }));
    expect(invoke).toHaveBeenCalledWith("set_pending_job_state", { projectId, jobId: queuedJob.job.id, state: "queued" });
    await user.click(await screen.findByRole("button", { name: "Cancel pending job" }));
    expect(invoke).toHaveBeenCalledWith("set_pending_job_state", { projectId, jobId: queuedJob.job.id, state: "cancelled" });
  });

  it("reviews a pending approval, requests interruption, and explicitly recovers the selected attempt", async () => {
    const approvalId = "approval-1";
    const attempt = runningJob.job.latestAttempt!;
    let inspection: JobInspection = {
      ...runningJob,
      job: { ...runningJob.job, latestAttempt: null },
      blockers: [{ type: "approval", approvalId }],
    };
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "job_failure") return {
        outcome: "interrupted",
        checkoutId,
        checkoutRoot: checkout.checkoutRoot,
        observation: { phase: "execution", cause: "shutdown", process: { code: 1, cancelled: true } },
        providerOutcome: "interrupted",
        logsAvailable: true,
        savedDiffAvailable: false,
      };
      if (command === "pending_job_approval_request") return { instructions: "Inspect the exact queued task." };
      if (command === "decide_job_approval") inspection = runningJob;
      if (command === "interrupt_queued_job_attempt") {
        inspection = { ...runningJob, job: { ...runningJob.job, latestAttempt: { ...attempt, cancelRequested: true } } };
      }
      if (command === "project_job_queue") {
        return { settings: { paused: false, capacity: 2 }, active: 0, jobs: [inspection] };
      }
    });
    const user = userEvent.setup();
    renderQueue();
    await user.click(await screen.findByRole("button", { name: "Agent job" }));
    expect(await screen.findByText("Inspect the exact queued task.")).toBeDefined();
    expect(invoke).toHaveBeenCalledWith("pending_job_approval_request", { projectId, approvalId });
    await user.click(screen.getByRole("button", { name: "Approve job" }));
    expect(invoke).toHaveBeenCalledWith("decide_job_approval", { projectId, approvalId, approved: true });
    await user.click(await screen.findByRole("button", { name: "Interrupt attempt" }));
    const identity = { projectId, jobId: runningJob.job.id, runId: attempt.runId };
    expect(invoke).toHaveBeenCalledWith("interrupt_queued_job_attempt", identity);
    expect((await screen.findByRole<HTMLButtonElement>("button", { name: "Interrupting…" })).disabled).toBe(true);

    inspection = {
      ...runningJob,
      job: { ...runningJob.job, latestAttempt: { ...attempt, status: "interrupted" } },
      recoveryActions: ["retry", "resume"],
    };
    await user.click(screen.getByRole("button", { name: "Refresh queue" }));
    expect(await screen.findByText("Saved diff is unavailable.")).toBeDefined();
    expect(screen.getByText("OmniAgent interrupted the attempt during exit.")).toBeDefined();
    await user.click(await screen.findByRole("button", { name: "Retry saved input" }));
    expect(invoke).toHaveBeenCalledWith("queue_job_recovery", { ...identity, action: "retry" });
    await user.click(screen.getByRole("button", { name: "Resume from checkpoint" }));
    expect(invoke).toHaveBeenCalledWith("queue_job_recovery", { ...identity, action: "resume" });
  });
});
