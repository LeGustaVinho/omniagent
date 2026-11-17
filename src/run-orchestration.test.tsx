// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { RunOrchestrationPanel } from "@/run-orchestration";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("opens actual routing, context decisions, and saved sources in the same inspector", async () => {
  const scope = { projectId: "project", conversationId: "chat", runId: "run" };
  const navigation = { openActivity: vi.fn(), openWorkspace: vi.fn() };
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "run_orchestration":
        return {
          job: {
            id: "job", kind: "agent", taskId: "task", ownerTaskId: "task", origin: { type: "pipeline" },
            operatorRequested: false, stage: { nodeInstanceId: "read_only_review", pipelineRunId: "flow", planRevision: "plan-02" },
          },
          route: { mode: "read_only", questionRoute: "native" },
          stagePin: { workflowRevision: "workflow-02", instanceRevision: "node-02", definitionRevision: 4, schedulerProfile: "read_only" },
          policy: {
            revision: "policy-at-admission",
            evidence: {
              role: "pipeline", permission: "allow", source: { kind: "project_rule", workspace: { type: "project" } },
              requirements: { capabilities: [], customizations: [] },
            },
          },
          context: {
            role: "reviewer", policyRevision: "context-02", usedBytes: 200, availableBytes: 800,
            decisions: [{ title: "Plan note", reason: "selected_by_rule", encodedBytes: 200, selectionOrder: 0 }],
          },
          document: { path: "notes/release.md", checkoutId: "checkout", revisionId: "old-note", sha256: "old-hash" },
          goal: {
            revisionId: "goal-revision", continuationOwner: "native", controlState: "active", blocker: "Waiting for input",
            checkpoint: null, attempt: { runId: "run", conversationId: "chat" },
            definition: {
              objective: "Verify the release", criteria: ["Check the selected tree"], maxTurns: 2, maxSeconds: 200, tokenBudget: 1000,
            },
            evidence: {
              provider: "codex",
              usage: { eventSequence: "17", inputTokens: 200, outputTokens: 20, cachedInputTokens: null },
              nativeStart: { eventSequence: "10", outcome: "confirmed", frame: '{"method":"thread/goal/set","params":{"objective":"Verify the release"}}' },
              nativeTurns: [{ turnId: "native-turn", startedEvent: "11", finishedEvent: null, outcome: null }],
              nativeObservation: {
                eventSequence: "18", observation: {
                  state: "active", reportedState: null, tokensUsed: 220, tokenBudget: 1000, elapsedSeconds: null,
                  evaluation: { evaluator: "Codex evaluator", verdict: "needs_checks", reason: "Required evidence is still missing" },
                },
              },
            },
          },
        };
      case "read_repository_markdown":
        return { markdown: "Original Markdown saved for the run" };
      case "conversation_history_inspection":
        return { previous: [{ contextId: "old-conversation-context", supersededBy: "new-context", messageCount: 1,
          replacedAt: "2026-11-16T19:00:00.000Z", action: "rewind" }], fork: null, retainedQuestions: [] };
      case "conversation_history_revision":
        return { contextId: "old-conversation-context", messages: [{ messageId: "saved", revisionId: "saved-revision", role: "user",
          text: "Dialogue from the superseded context" }] };
      case "goal_verification":
        return null;
      case "attempt_worktree_evidence":
        return {
          kind: "saved",
          artifact: {
            revision: "candidate-old", committedPatchBase64: btoa("+saved change\n"),
            start: { starting: { head: "old-head", changes: null }, worktree: { ownerTaskId: "task", branch: "task-branch" } },
            candidate: { head: "candidate-head", changes: null },
          },
        };
      default:
        throw new Error(command);
    }
  });
  render(<QueryClientProvider client={new QueryClient()}>
    <RunOrchestrationPanel scope={scope} live={false} navigation={navigation} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByText("Orchestration and worktree"));
  expect(await screen.findByText("policy-at-admission")).toBeTruthy();
  expect(screen.getByText("node-02 · Definition 4")).toBeTruthy();
  expect(screen.getByText("reviewer · 200 of 800 bytes")).toBeTruthy();
  expect(screen.getByText("Waiting for input")).toBeTruthy();
  expect(screen.getByText("Attempt usage · Codex event 17")).toBeTruthy();
  await user.click(screen.getByText("Goal criteria"));
  expect(screen.getByText("Check the selected tree")).toBeTruthy();
  await user.click(screen.getByText("Exact application goal command"));
  expect(screen.getByText(/thread\/goal\/set/).tagName).toBe("PRE");
  await user.click(screen.getByText("Native goal evidence · Codex"));
  expect(screen.getByText("Provider evaluator · Codex evaluator")).toBeTruthy();
  expect(screen.getByText("native-turn")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("run_orchestration", scope);
  await user.click(screen.getByText("Context selection reasons"));
  expect(screen.getByText(/selected by rule · 200 bytes · Included 1/)).toBeTruthy();
  await user.click(screen.getByText("Saved Markdown · notes/release.md"));
  expect(await screen.findByText("Original Markdown saved for the run")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("read_repository_markdown", { checkoutId: "checkout", revisionId: "old-note" });
  await user.click(screen.getByText("Saved candidate for this attempt"));
  await user.click(await screen.findByText("Changes committed during the attempt"));
  const patch = await screen.findByText("+saved change");
  expect(patch.tagName).toBe("PRE");
  expect(patch.isContentEditable).toBeFalsy();
  await user.click(screen.getByRole("button", { name: "Open workflow" }));
  expect(navigation.openWorkspace).toHaveBeenCalledWith("pipelines", "run:flow");
  await user.click(screen.getByText("Context revisions"));
  await user.click(await screen.findByRole("button", { name: /Before rewind/ }));
  expect(await screen.findByText("Dialogue from the superseded context")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("conversation_history_revision", { conversationId: "chat", contextId: "old-conversation-context" });
});

it("opens delivery controls for a historical attempt without a graph pin", async () => {
  const scope = { projectId: "project", runId: "old-attempt" };
  const navigation = { openActivity: vi.fn(), openWorkspace: vi.fn() };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command !== "run_orchestration") throw new Error(command);
    return {
      job: {
        id: "job", kind: "agent", taskId: null, ownerTaskId: null, origin: { type: "pipeline" },
        operatorRequested: false, stage: { nodeInstanceId: "plan", pipelineRunId: "legacy-delivery" },
      },
      stagePin: null,
    };
  });
  render(<QueryClientProvider client={new QueryClient()}>
    <RunOrchestrationPanel scope={scope} live={false} navigation={navigation} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByText("Orchestration and worktree"));
  await user.click(await screen.findByRole("button", { name: "Open workflow" }));
  expect(navigation.openWorkspace).toHaveBeenCalledWith("pipelines", "legacy-delivery");
});

it("shows the Epic, Task, and Sub-Task description revisions pinned to the inspected attempt", async () => {
  const scope = { projectId: "project", conversationId: "chat", runId: "historical-run" };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command !== "run_orchestration") throw new Error(command);
    return {
      job: null, route: null, stagePin: null, policy: null, context: null, document: null, goal: null, unit: null,
      taskDescriptions: {
        epic: { id: "epic-revision-at-dispatch", revision: 5, markdown: "Release the original scope" },
        task: { id: "task-revision-at-dispatch", revision: 7, markdown: "Retain the published API" },
        subtask: { id: "subtask-revision-at-dispatch", revision: 2, markdown: "- [x] Check the response schema" },
      },
    };
  });
  render(<QueryClientProvider client={new QueryClient()}>
    <RunOrchestrationPanel scope={scope} live={false} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByText("Orchestration and worktree"));
  await user.click(await screen.findByText("Saved work descriptions"));
  for (const [heading, revision, markdown] of [
    ["Epic description · Revision 5", "epic-revision-at-dispatch", "Release the original scope"],
    ["Task description · Revision 7", "task-revision-at-dispatch", "Retain the published API"],
    ["Sub-Task description · Revision 2", "subtask-revision-at-dispatch", "- [x] Check the response schema"],
  ] as const) {
    expect(screen.getByRole("heading", { name: heading })).toBeTruthy();
    expect(screen.getByText(revision).tagName).toBe("CODE");
    const source = screen.getByText(markdown);
    expect(source.tagName).toBe("PRE");
    expect(source.isContentEditable).toBeFalsy();
  }
  expect(screen.queryByRole("checkbox")).toBeNull();
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(invoke).toHaveBeenCalledWith("run_orchestration", scope);
});
