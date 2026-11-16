// @vitest-environment jsdom
import { type ComponentProps } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ModelPicker } from "@/components/model-picker";
import { ConversationPanel } from "@/conversation-panels";
import type { ConversationGoalOverview, GoalDefinition, GoalTransitionReview } from "@/conversation-goal";
import { RunInspectorNavigation } from "@/run-inspector-navigation";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/components/model-picker", async (load) => ({
  ...(await load<typeof import("@/components/model-picker")>()),
  ModelPicker: ({ options, selection }: ComponentProps<typeof ModelPicker>) => options.map((option) =>
    <button key={option.name} type="button" onClick={() => {
      if (selection.mode === "single") selection.onChange(option.selection);
    }}>{option.name}</button>),
}));
afterEach(() => { cleanup(); vi.resetAllMocks(); vi.unstubAllGlobals(); });

function openPanel(initialDraft: string, native: boolean) {
  const conversationId = `goal-${native ? "native" : "managed"}`;
  const projectId = "project-from-goal";
  const selection = { provider: "codex", profileId: "desktop-profile", modelId: "selected-goal-model" };
  const runId = "owning-goal-attempt";
  const active = vi.fn();
  let draft = initialDraft;
  let value: ConversationGoalOverview | null = null;
  function review(goal: ConversationGoalOverview["goal"], controlState: "paused" | "needs_review"): GoalTransitionReview {
    return {
      revisionId: goal.id, definition: goal.definition, continuationOwner: goal.continuationOwner,
      controlState, checkpoint: { ordinal: 2, runId, nextAction: "continue" },
      checkpointContextId: "saved-checkpoint", checkpointRunId: runId, reviewToken: "sealed-resume",
      stopReviewRunId: controlState === "needs_review" ? runId : null,
      scope: { projectId, checkoutId: "checkout-original", checkoutRoot: "D:/release", provider: selection.provider,
        profileId: selection.profileId, model: selection.modelId, effort: null, mode: "read_only" },
    };
  }
  function recoverAfterRestart() {
    if (!value) throw new Error("No goal to recover");
    value = { ...value, goal: { ...value.goal, controlState: "needs_review" }, transition: review(value.goal, "needs_review") };
  }
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "conversation_draft": return draft;
      case "conversation_history": return { contextId: "saved-context", messages: [] };
      case "save_conversation_draft": { draft = (args as { text: string }).text; return; }
      case "conversation_models": return [{ option: { name: "Selected model", selection, selectable: true }, efforts: [] }];
      case "latest_conversation_run":
      case "latest_conversation_job": return null;
      case "pending_conversation_instructions": return [];
      case "composer_suggestion_mode": return "off";
      case "conversation_goal_overview": return value;
      case "start_conversation_goal": {
        const input = args as { definition: GoalDefinition; submittedDraft: string | null };
        if (input.submittedDraft !== null && input.submittedDraft !== draft) throw new Error("Stale draft");
        if (input.submittedDraft !== null) draft = "";
        value = {
          goal: { id: "goal-revision", definition: input.definition, continuationOwner: native ? "native" : "managed", controlState: "active",
            provider: "codex", model: selection.modelId, job: { projectId }, checkpoint: null, verification: null },
          activity: { blocker: null, attempt: { runId, conversationId }, evidence: native ? {
            nativeObservation: { observation: { state: "active", tokensUsed: 234, tokenBudget: null, elapsedSeconds: 7 } }, usage: null,
          } : null },
          managedReason: native ? null : "OmniAgent enforces the requested turn limit.", history: [], transition: null,
        };
        return value.goal;
      }
      case "request_goal_stop": {
        const action = (args as { action: string }).action;
        if (value) value = {
          ...value, goal: { ...value.goal, controlState: action === "pause" ? "paused" : "cancelled" },
          transition: action === "pause" ? review(value.goal, "paused") : null,
        };
        return value?.goal.controlState;
      }
      case "confirm_native_goal_stop": {
        if ((args as { runId: string }).runId !== runId) throw new Error("Wrong native executor");
        if (value) value = { ...value, goal: { ...value.goal, controlState: "paused" }, transition: review(value.goal, "paused") };
        return "paused";
      }
      case "resume_goal": {
        if (value) value = { ...value, goal: { ...value.goal, controlState: "active" }, transition: null };
        return;
      }
      default: throw new Error(`Unexpected IPC ${command}`);
    }
  });
  const props = {
    params: { conversationId, title: "Release conversation" },
    api: { isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }) },
  } as unknown as ComponentProps<typeof ConversationPanel>;
  const mount = () => render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <RunInspectorNavigation.Provider value={{ openActivity: active, activeRunId: undefined }}>
      <ConversationPanel {...props} />
    </RunInspectorNavigation.Provider>
  </QueryClientProvider>);
  return { mount, selection, conversationId, projectId, runId, active, recoverAfterRestart };
}

it("submits a slash goal and reviews the original native executor after reopening", async () => {
  const submittedDraft = "/goal Verify the release artifacts";
  const fixture = openPanel(submittedDraft, true);
  const initial = fixture.mount();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Selected model" }));
  await user.click(await screen.findByRole("button", { name: "Start goal" }));
  const goal = within(await screen.findByRole("region", { name: "Conversation goal" }));
  expect(goal.getByText("Verify the release artifacts")).toBeTruthy();
  expect(goal.getByText("Native")).toBeTruthy();
  expect(goal.getByText("234")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("start_conversation_goal", {
    conversationId: fixture.conversationId, expectedContextId: "saved-context", selection: fixture.selection, effort: null,
    definition: { objective: "Verify the release artifacts", criteria: [], maxTurns: null, maxSeconds: null, tokenBudget: null }, submittedDraft,
  });
  expect((screen.getByRole("textbox", { name: "Message draft" }) as HTMLTextAreaElement).value).toBe("");
  initial.unmount();
  fixture.recoverAfterRestart();
  fixture.mount();
  const reopened = within(await screen.findByRole("region", { name: "Conversation goal" }));
  expect(reopened.getByText("Needs review")).toBeTruthy();
  expect(reopened.getByText("234")).toBeTruthy();
  expect(reopened.queryByRole("button", { name: "Resume goal" })).toBeNull();
  await user.click(reopened.getByRole("button", { name: "Review executor stop" }));
  const stop = within(reopened.getByRole("region", { name: "Review executor stop" }));
  expect(stop.getByText(fixture.runId)).toBeTruthy();
  await user.click(stop.getByRole("button", { name: "Verify saved stop evidence" }));
  expect(invoke).toHaveBeenCalledWith("confirm_native_goal_stop", { projectId: fixture.projectId, revisionId: "goal-revision", runId: fixture.runId });
  await user.click(await reopened.findByRole("button", { name: "Resume goal" }));
  const checkpoint = within(reopened.getByRole("region", { name: "Review goal resume" }));
  expect(checkpoint.getByText("Resume from this checkpoint in a fresh provider session.")).toBeTruthy();
  await user.click(checkpoint.getByRole("button", { name: "Confirm goal resume" }));
  expect(await reopened.findByText("Active")).toBeTruthy();
  await user.click(await reopened.findByRole("button", { name: "Cancel goal" }));
  expect(await reopened.findByText("Cancelled")).toBeTruthy();
});

it("opens bare goal controls, persists configured limits, and restores stopped goal evidence on reopen", async () => {
  const fixture = openPanel("/goal", false);
  const initial = fixture.mount();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Open Goal Mode" }));
  const modal = within(screen.getByRole("dialog", { name: "Goal Mode" }));
  await user.type(modal.getByRole("textbox", { name: "Objective" }), "Prepare the release");
  await user.type(modal.getByRole("textbox", { name: /Criteria/ }), "Check the Windows package\nReview changelog");
  await user.type(modal.getByRole("spinbutton", { name: "Turn limit" }), "3");
  await user.click(modal.getByRole("button", { name: "Selected model" }));
  await waitFor(() => expect((modal.getByRole("button", { name: "Start goal" }) as HTMLButtonElement).disabled).toBe(false));
  await user.click(modal.getByRole("button", { name: "Start goal" }));
  const goal = within(await screen.findByRole("region", { name: "Conversation goal" }));
  expect(goal.getByText("Check the Windows package")).toBeTruthy();
  expect(goal.getByText("OmniAgent enforces the requested turn limit.")).toBeTruthy();
  await user.click(goal.getByRole("button", { name: "Pause goal" }));
  expect(await goal.findByText("Paused")).toBeTruthy();
  await user.click(goal.getByRole("button", { name: "Resume goal" }));
  const review = within(goal.getByRole("region", { name: "Review goal resume" }));
  expect(review.getByText("Prepare the release")).toBeTruthy();
  expect(review.getByText(/Checkpoint 2/)).toBeTruthy();
  expect(review.getByText("D:/release")).toBeTruthy();
  expect(review.getByText(fixture.selection.modelId)).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.some(([name]) => name === "resume_goal")).toBe(false);
  await user.click(review.getByRole("button", { name: "Confirm goal resume" }));
  expect(invoke).toHaveBeenCalledWith("resume_goal", { projectId: fixture.projectId, revisionId: "goal-revision", reviewToken: "sealed-resume" });
  expect(await goal.findByText("Active")).toBeTruthy();
  await user.click(goal.getByRole("button", { name: "Cancel goal" }));
  expect(await goal.findByText("Cancelled")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("request_goal_stop", { projectId: fixture.projectId, revisionId: "goal-revision", action: "cancel" });
  initial.unmount();
  fixture.mount();
  const reopened = within(await screen.findByRole("region", { name: "Conversation goal" }));
  expect(reopened.getByText("Cancelled")).toBeTruthy();
  await user.click(reopened.getByRole("button", { name: "Open goal activity" }));
  expect(fixture.active).toHaveBeenCalledWith(fixture.runId, fixture.conversationId);
});
