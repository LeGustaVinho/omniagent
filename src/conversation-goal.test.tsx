// @vitest-environment jsdom
import { type ComponentProps } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ModelPicker } from "@/components/model-picker";
import { ConversationPanel } from "@/conversation-panels";
import type { ConversationGoalOverview, GoalDefinition } from "@/conversation-goal";
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
          managedReason: native ? null : "OmniAgent enforces the requested turn limit.", history: [],
        };
        return value.goal;
      }
      case "request_goal_stop": {
        const action = (args as { action: string }).action;
        if (value) value = { ...value, goal: { ...value.goal, controlState: action === "pause" ? "paused" : "cancelled" } };
        return value?.goal.controlState;
      }
      case "resume_goal": {
        if (value) value = { ...value, goal: { ...value.goal, controlState: "active" } };
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
  return { mount, selection, conversationId, projectId, runId, active };
}

it("submits a slash goal through the selected profile and displays native usage", async () => {
  const submittedDraft = "/goal Verify the release artifacts";
  const fixture = openPanel(submittedDraft, true);
  fixture.mount();
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
