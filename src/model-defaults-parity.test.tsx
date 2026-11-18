// @vitest-environment jsdom

import type { ComponentProps } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ConversationPanel } from "@/conversation-panels";
import { ConversationModelSettings, ExecutionProfileModelSettings } from "@/model-default-settings";
import { CHECKOUT_MODEL_DEFAULTS, type ModelDefaultPurpose, type ProjectModelDefaults } from "@/model-defaults";
import { TaskAssignment } from "@/task-assignment";
import { useWorkspace } from "@/workspace-state";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => {
  cleanup(); useWorkspace.getState().selectCheckout(null); vi.unstubAllGlobals(); vi.resetAllMocks();
});

const checkout = { projectId: "local", checkoutId: "checkout", repositoryRoot: "D:\\repository", checkoutRoot: "D:\\repository" };
const codex: ModelOption = { selection: { provider: "codex", profileId: "codex-profile", modelId: "reported-shared-id" },
  name: "Codex rated model", selectable: true, unavailableReason: null, catalogVersion: 1,
  rating: { intelligenceTenths: 915, priceScore: 72, apiReferenceCents: 125 } };
const claude: ModelOption = { selection: { provider: "claude", profileId: "claude-profile", modelId: codex.selection.modelId },
  name: "Claude rated model", selectable: true, unavailableReason: null, catalogVersion: 1,
  rating: { intelligenceTenths: 833, priceScore: 81, apiReferenceCents: 90 } };

function fixture({ rejectProjectSave = false, checkoutDefaults }: {
  rejectProjectSave?: boolean;
  checkoutDefaults?: Promise<ProjectModelDefaults>;
} = {}) {
  let defaults: ProjectModelDefaults = { conversation: null, planning: null, implementation: null };
  const byProfile = new Map<string, ModelSelection>();
  let assignment: ModelSelection | null = null;
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    const args = input as { provider?: string; profileId?: string; selection?: ModelSelection | null; purpose?: ModelDefaultPurpose };
    switch (command) {
      case "conversation_models": return [codex, claude].filter(({ selection }) => selection.provider === args.provider)
        .map((option) => ({ option, efforts: [], defaultEffort: null }));
      case "checkout_model_defaults": return checkoutDefaults ?? defaults;
      case "project_model_defaults": return defaults;
      case "set_project_model_default": {
        if (rejectProjectSave) throw new Error("The selected model is no longer available");
        defaults = { ...defaults, [args.purpose!]: args.selection ?? null };
        return defaults;
      }
      case "execution_profile_default_model": return byProfile.get(args.profileId!) ?? null;
      case "set_execution_profile_default_model": {
        if (args.selection) byProfile.set(args.profileId!, args.selection);
        else byProfile.delete(args.profileId!);
        return args.selection ?? null;
      }
      case "set_project_task_assignment": assignment = args.selection ?? null; return assignment ? [{ ...assignment, role: "implementer" }] : [];
      case "project_task_assignments": return assignment ? [{ ...assignment, role: "implementer" }] : [];
      case "conversation_attachment_draft": return { revision: null, files: [] };
      case "checkout_conversation": return { id: "chat", title: "Current conversation", checkoutId: checkout.checkoutId, contextId: "context" };
      case "conversation_reference_draft": case "pending_conversation_instructions": case "conversation_side_requests":
      case "list_conversations": case "saved_conversation_questions": return [];
      case "latest_conversation_run": case "latest_conversation_job": case "conversation_goal_overview": return null;
      case "conversation_draft": return "";
      case "conversation_history": return { contextId: "context", messages: [] };
      case "composer_suggestion_mode": return "off";
      case "conversation_customization_options": return [];
      default: throw new Error(`Unexpected ${command}`);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const props = { params: { conversationId: "chat", title: "Current conversation" }, api: {
    isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }),
  } } as unknown as ComponentProps<typeof ConversationPanel>;
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  useWorkspace.getState().selectCheckout(checkout);
  render(<QueryClientProvider client={client}>
    <ConversationPanel {...props} />
    <section aria-label="Project defaults"><ConversationModelSettings projectId={checkout.projectId} /></section>
    <section aria-label="Provider defaults"><ExecutionProfileModelSettings provider="claude" /></section>
    <section aria-label="Agile work"><TaskAssignment projectId={checkout.projectId} taskId="task" /></section>
  </QueryClientProvider>);
  return { client, user: userEvent.setup() };
}

async function chooseAndMeasure(user: ReturnType<typeof userEvent.setup>, label: RegExp, option: ModelOption, price: string) {
  await user.click(await screen.findByRole("button", { name: label }));
  const row = await screen.findByRole("menuitemradio", { name: option.name });
  await user.hover(row);
  const ratings = within(await screen.findByLabelText(`Ratings for ${option.name}`));
  const values = option.rating!;
  expect(ratings.getByLabelText("Intelligence").getAttribute("value")).toBe(String(values.intelligenceTenths / 10));
  expect(ratings.getByLabelText("Price score").getAttribute("value")).toBe(String(values.priceScore));
  expect(ratings.getByText(`API reference ${price} / 1M tokens`)).toBeTruthy();
  await user.click(row);
}

it("uses identical rating meters in live conversation, project settings and Agile and preserves exact profiles on a switch", async () => {
  const { user } = fixture();
  await chooseAndMeasure(user, /^Model · /, codex, "$1.25");
  await chooseAndMeasure(user, /^Conversation model · /, codex, "$1.25");
  expect(invoke).toHaveBeenCalledWith("set_project_model_default", { projectId: checkout.projectId, purpose: "conversation", selection: codex.selection });
  const agile = within(screen.getByRole("region", { name: "Agile work" }));
  await user.click(agile.getByText("Planned model assignments"));
  await chooseAndMeasure(user, /^Assignment model · /, codex, "$1.25");
  expect(invoke).toHaveBeenCalledWith("set_project_task_assignment", { projectId: checkout.projectId, taskId: "task", role: "implementer", selection: codex.selection });

  const profile = within(screen.getByRole("region", { name: "Provider defaults" }));
  await user.click(profile.getByText("Execution profile model"));
  const profileTrigger = await profile.findByRole<HTMLButtonElement>("button", { name: /^Claude Code profile default model · / });
  await waitFor(() => expect(profileTrigger.disabled).toBe(false));
  await chooseAndMeasure(user, /^Claude Code profile default model · /, claude, "$0.90");
  expect(invoke).toHaveBeenCalledWith("set_execution_profile_default_model", { provider: "claude", profileId: "claude-profile", selection: claude.selection });

  await user.selectOptions(agile.getByRole("combobox", { name: "Agent provider" }), "claude");
  expect(await agile.findByRole("button", { name: "Assignment model · Choose model" })).toBeTruthy();
  await chooseAndMeasure(user, /^Assignment model · /, claude, "$0.90");
  expect(invoke).toHaveBeenCalledWith("set_project_task_assignment", { projectId: checkout.projectId, taskId: "task", role: "implementer", selection: claude.selection });
  expect(await screen.findByRole("button", { name: `Model · ${codex.name}` })).toBeTruthy();
  await chooseAndMeasure(user, /^Model · /, claude, "$0.90");
  expect(await screen.findByRole("button", { name: `Model · ${claude.name}` })).toBeTruthy();
});

it("shows an availability error from the settings authority and keeps the saved selection", async () => {
  const { user } = fixture({ rejectProjectSave: true });
  await chooseAndMeasure(user, /^Conversation model · /, codex, "$1.25");
  const project = within(screen.getByRole("region", { name: "Project defaults" }));
  expect((await project.findByRole("alert")).textContent).toContain("The selected model is no longer available");
  expect(project.getByRole("button", { name: "Conversation model · Use profile default" })).toBeTruthy();
});

it("copies and resets the clicked saved model default at the correct scope", async () => {
  const { user } = fixture();
  await chooseAndMeasure(user, /^Conversation model · /, codex, "$1.25");
  const setting = within(screen.getByRole("region", { name: "Project defaults" }));
  const label = setting.getByText("Conversation model", { selector: "label" });
  fireEvent.contextMenu(label);
  await user.click(screen.getByRole("menuitem", { name: "Copy saved profile ID" }));
  expect(await navigator.clipboard.readText()).toBe(codex.selection.profileId);
  fireEvent.contextMenu(label);
  await user.click(screen.getByRole("menuitem", { name: "Use profile default" }));
  expect(invoke).toHaveBeenCalledWith("set_project_model_default", { projectId: checkout.projectId, purpose: "conversation", selection: null });
  expect(await setting.findByRole("button", { name: "Conversation model · Use profile default" })).toBeTruthy();
});

it("keeps the explicit conversation selection when the project model arrives later", async () => {
  let resolveDefaults!: (value: ProjectModelDefaults) => void;
  const checkoutDefaults = new Promise<ProjectModelDefaults>((resolve) => { resolveDefaults = resolve; });
  const { client, user } = fixture({ checkoutDefaults });
  await chooseAndMeasure(user, /^Model · /, codex, "$1.25");
  expect(invoke).toHaveBeenCalledWith("checkout_model_defaults", { checkoutId: checkout.checkoutId });

  const defaults = { conversation: claude.selection, planning: null, implementation: null };
  await act(async () => { resolveDefaults(defaults); });
  await waitFor(() => expect(client.getQueryData([...CHECKOUT_MODEL_DEFAULTS, checkout.checkoutId])).toEqual(defaults));
  expect(screen.getByRole("button", { name: `Model · ${codex.name}` })).toBeTruthy();
});

it("uses the checkout's applicable project default in the conversation and Plan Mode", async () => {
  const { user } = fixture({ checkoutDefaults: Promise.resolve({
    conversation: claude.selection, planning: codex.selection, implementation: null,
  }) });
  expect(await screen.findByRole("button", { name: `Model · ${claude.name}` })).toBeTruthy();
  await user.selectOptions(screen.getByRole("combobox", { name: "Conversation mode" }), "plan");
  expect(await screen.findByRole("button", { name: `Model · ${codex.name}` })).toBeTruthy();
});
