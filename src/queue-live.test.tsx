// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { JobInspection } from "@/bindings/JobInspection";
import QueuePanel from "@/queue-panel";
import type { QueueJobFocus, WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const projectId = "project-1";
const checkoutId = "checkout-1";
const checkout = { projectId, checkoutId, repositoryRoot: "C:/repository", checkoutRoot: "C:/repository" };

const queuedJob: JobInspection = {
  job: {
    id: "job-2", projectId, checkoutId, taskId: null, worktreeOwnerTaskId: null, origin: { type: "operator" },
    context: { conversationId: "conversation-2", contextId: "context-2" }, mode: "read_only",
    priority: 10, queueState: "queued", latestAttempt: null, queuedRecovery: null, downstreamWait: null, pipeline: null, kind: "agent",
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

function renderQueue(overrides: Partial<WorkspaceProps> & { active?: boolean; focusJob?: QueueJobFocus } = {}) {
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
  it("operates on the job under the context menu while another job is selected", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => command === "project_job_queue"
      ? { settings: { paused: false, capacity: 3 }, active: 1, jobs: [queuedJob, runningJob] } : undefined);
    const user = userEvent.setup();
    renderQueue();
    await user.click(await screen.findByRole("button", { name: "Agent job" }));
    fireEvent.contextMenu(screen.getByRole("button", { name: "Conversation turn" }));
    const menu = within(await screen.findByRole("menu", { name: "Conversation turn job-2" }));
    expect(menu.queryByRole("menuitem", { name: "Interrupt attempt" })).toBeNull();
    await user.click(menu.getByRole("menuitem", { name: "Pause job" }));
    expect(invoke).toHaveBeenCalledWith("set_pending_job_state", { projectId, jobId: queuedJob.job.id, state: "paused" });
    expect(within(screen.getByRole("complementary", { name: "Selected job" })).getByRole("heading", { name: "Agent job" })).toBeTruthy();
  });

  it("moves keyboard focus between a job, its details, and the active filter", async () => {
    vi.mocked(invoke).mockResolvedValue({ settings: { paused: false, capacity: 3 }, active: 1, jobs: [queuedJob, runningJob] });
    const user = userEvent.setup();
    renderQueue();
    const job = await screen.findByRole("button", { name: "Agent job" });
    job.focus();
    await user.keyboard("{Enter}");

    const details = within(screen.getByRole("complementary", { name: "Selected job" }));
    const heading = details.getByRole("heading", { name: "Agent job" });
    await waitFor(() => expect(document.activeElement).toBe(heading));
    await user.tab();
    expect(document.activeElement).toBe(details.getByRole("button", { name: "Close job details" }));
    await user.keyboard("{Enter}");
    expect(document.activeElement).toBe(job);

    await user.keyboard("{Enter}");
    const history = within(screen.getByRole("navigation", { name: "Queue states" })).getByRole("button", { name: "History" });
    await user.click(history);
    expect(screen.queryByRole("button", { name: "Agent job" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Close job details" }));
    expect(document.activeElement).toBe(history);
  });

  it.each([false, true])("queues an operator-selected model with reference scores (subagent %s)", async (child) => {
    const selection = { provider: "codex", profileId: "desktop-direct:codex", modelId: "gpt-6-luna" };
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "project_job_queue") return { settings: { paused: false, capacity: 3 }, active: 1, jobs: [runningJob] };
      if (command === "conversation_models") return [{ option: {
        selection, name: "GPT‑6 Luna", selectable: true, unavailableReason: null, catalogVersion: 1,
        rating: { intelligenceTenths: 169, apiReferenceCents: 20, priceScore: 12 },
      } }];
      if (command === "enqueue_managed_agent_job") return queuedJob.job;
    });
    const user = userEvent.setup();
    renderQueue();
    if (child) {
      await user.click(await screen.findByRole("button", { name: "Agent job" }));
      await user.click(screen.getByRole("button", { name: "Add subagent" }));
      expect(screen.queryByRole("option", { name: "Repository write" })).toBeNull();
    } else {
      await user.click(screen.getByRole("button", { name: "Add job" }));
    }
    await user.type(screen.getByLabelText("Instructions"), "Review the generated candidate.");
    await user.selectOptions(screen.getByLabelText("Operation"), "plan");
    await user.click(await screen.findByRole("button", { name: "Model · Choose model" }));
    const picker = screen.getByRole("menu", { name: "Model" });
    expect(within(picker).getByText("16.9/100")).toBeDefined();
    expect(within(picker).getByText("12/100")).toBeDefined();
    await user.click(within(picker).getByRole("menuitemradio", { name: "GPT‑6 Luna" }));
    await user.click(screen.getByRole("button", { name: child ? "Queue subagent" : "Queue job" }));
    expect(invoke).toHaveBeenCalledWith("enqueue_managed_agent_job", {
      projectId, checkoutId, parentRunId: child ? "run-1" : null, selection,
      mode: "plan", instructions: "Review the generated candidate.",
    });
  });

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

  it("opens the requested job and its approval only in the matching project", async () => {
    const approval = { ...queuedJob, blockers: [{ type: "approval", approvalId: "exact-approval" }] } satisfies JobInspection;
    vi.mocked(invoke).mockImplementation(async (command) => command === "pending_job_approval_request"
      ? { instructions: "Review only this queued job." }
      : { settings: { paused: false, capacity: 2 }, active: 1, jobs: [runningJob, approval] });

    renderQueue({ focusJob: { projectId: "foreign-project", jobId: approval.job.id } });
    await screen.findByRole("button", { name: "Conversation turn" });
    expect(screen.queryByRole("complementary", { name: "Selected job" })).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith("pending_job_approval_request", expect.anything());
    cleanup();

    renderQueue({ focusJob: { projectId, jobId: approval.job.id } });
    const selected = within(await screen.findByRole("complementary", { name: "Selected job" }));
    expect(selected.getByText("Waiting for approval exact-approval.")).toBeTruthy();
    expect(await selected.findByText("Review only this queued job.")).toBeTruthy();
    expect(invoke).toHaveBeenCalledWith("pending_job_approval_request", { projectId, approvalId: "exact-approval" });
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

  it("creates an example job with the same instructions, CLI, model and operation controls", async () => {
    const user = userEvent.setup();
    renderQueue({ simulation: { scenario: "delivery", epoch: 1, delivery: "queued", advanceDelivery: () => {} } });
    await user.click(screen.getByRole("button", { name: "Add job" }));
    const form = within(screen.getByRole("form", { name: "Managed job" }));
    const queueJob = form.getByRole<HTMLButtonElement>("button", { name: "Queue job" });

    await user.type(form.getByLabelText("Instructions"), "Review the generated candidate.");
    await user.selectOptions(form.getByLabelText("Operation"), "plan");
    await user.click(form.getByRole("button", { name: "Model · Choose model" }));
    await user.click(screen.getByRole("menuitemradio", { name: "GPT-6.1 Sol" }));
    expect(queueJob.disabled).toBe(false);
    await user.selectOptions(form.getByLabelText("CLI"), "claude");
    expect(queueJob.disabled).toBe(true);
    await user.click(form.getByRole("button", { name: "Model · Choose model" }));
    expect(screen.queryByRole("menuitemradio", { name: "GPT-6.1 Sol" })).toBeNull();
    await user.click(screen.getByRole("menuitemradio", { name: "Claude Sonnet 5.5" }));
    await user.click(queueJob);

    const details = within(screen.getByRole("complementary", { name: "Selected job" }));
    expect(details.getByRole("heading", { name: "Review the generated candidate." })).toBeTruthy();
    expect(details.getByText("Claude Sonnet 5.5")).toBeTruthy();
    expect(details.getByText("plan")).toBeTruthy();
    await user.click(details.getByRole("button", { name: "Pause job" }));
    expect(details.getByText("Paused")).toBeTruthy();
    await user.click(details.getByRole("button", { name: "Resume job" }));
    await user.click(details.getByRole("button", { name: "Cancel pending job" }));
    expect(details.getByText("Cancelled")).toBeTruthy();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("keeps the parent's execution scope when queuing an example subagent", async () => {
    const user = userEvent.setup();
    renderQueue({ simulation: { scenario: "delivery", epoch: 1, delivery: "queued", advanceDelivery: () => {} } });
    await user.click(screen.getByRole("button", { name: "Index repository notes" }));
    const details = within(screen.getByRole("complementary", { name: "Selected job" }));
    await user.click(details.getByRole("button", { name: "Dispatch" }));
    await user.click(details.getByRole("button", { name: "Add subagent" }));

    const form = within(screen.getByRole("form", { name: "Managed subagent" }));
    expect(form.queryByRole("option", { name: "Repository write" })).toBeNull();
    await user.type(form.getByLabelText("Instructions"), "Check the release links.");
    await user.click(form.getByRole("button", { name: "Model · Choose model" }));
    await user.click(screen.getByRole("menuitemradio", { name: "GPT-6.1 Sol" }));
    await user.click(form.getByRole("button", { name: "Queue subagent" }));
    expect(details.getByText("Operator · parent job Index repository notes")).toBeTruthy();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("links an unmet skill to the attempt that checked it", async () => {
    const rejected: JobInspection = {
      ...runningJob,
      job: { ...runningJob.job, latestAttempt: { ...runningJob.job.latestAttempt!, status: "failed" } },
      blockers: [{ type: "dispatch_rejected", rejection: {
        type: "requirements", capabilities: [], customizations: [{ type: "skill", id: "declared-skill" }],
      } }],
    };
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === "job_failure" ? null : { settings: { paused: false, capacity: 2 }, active: 0, jobs: [rejected] });
    const user = userEvent.setup();
    const { openActivity } = renderQueue();
    await user.click(await screen.findByRole("button", { name: "Agent job" }));
    const blockers = screen.getByRole("region", { name: "Admission blockers" });
    expect(within(blockers).getByText(/declared-skill/)).toBeDefined();
    await user.click(within(blockers).getByRole("button", { name: "Inspect unmet requirements" }));
    expect(openActivity).toHaveBeenCalledWith("run-1", "attempt-conversation");
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
