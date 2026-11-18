// @vitest-environment jsdom

import { invoke } from "@tauri-apps/api/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { CustomizationInventory } from "@/bindings/CustomizationInventory";
import { registerActiveCustomizationDraft } from "@/active-customization-draft";
import { hasComposerSkills, type ComposerCustomization } from "@/conversation-customization-picker";
import { readSourceLineTarget } from "@/file-context-actions";
import { readNoteSearchTarget } from "@/ide-file-search";
import { defaultAppearance } from "@/settings-appearance";
import SettingsDialog from "@/settings-dialog";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const checkout = { projectId: "project", checkoutId: "linked", checkoutRoot: "D:\\worktrees\\linked", repositoryRoot: "D:\\main" };
const id = "a".repeat(64);
const skillMarker = `@skill:${id}:${"b".repeat(64)}`;
const instructions = { file: { path: "docs\\AGENTS.md", revision: "instructions-revision" }, text: `Use the local fixture.\r\n${skillMarker}\r\n` };
const hook = {
  id: "hook-id", provider: "claude", kind: "hook", source: ".claude\\settings.json", sourceRevision: "source-sha",
  label: "SessionStart", target: "scripts/start.ps1", arguments: ["-NoProfile"], requestedScope: "Before session starts",
  localFiles: [{ path: "scripts\\start.ps1", revision: "script-sha" }], fingerprint: "fingerprint", review: "pending", issue: null,
} as const;
const inventory: CustomizationInventory = {
  workingDirectory: checkout.checkoutRoot, instructions: [instructions], entries: [{ ...hook, arguments: [...hook.arguments], localFiles: [...hook.localFiles] }],
  unresolvedSources: [".agents\\broken"],
};
const scope = { checkoutId: checkout.checkoutId, conversationId: "actual-chat", title: "Release review", mode: "plan" as const,
  selection: { provider: "codex" as const, profileId: "profile", modelId: "model" }, effort: "high" };
const skill: ComposerCustomization = {
  marker: skillMarker, availability: "filesystem", skill: {
    name: "check-links", id, provider: "codex", namespace: null, revision: "skill-revision",
    path: "\\\\?\\D:\\worktrees\\linked\\.agents\\skills\\check-links\\SKILL.md",
  },
};
const external = { ...skill, skill: { ...skill.skill, name: "External export", namespace: "plugin", path: "D:\\worktrees\\linked-other\\SKILL.md" },
  marker: `@skill:${"c".repeat(64)}:${"d".repeat(64)}` };

const registrations: (() => void)[] = [];
function selectDraft(value = scope) {
  const remove = registerActiveCustomizationDraft(value);
  registrations.push(remove);
  return remove;
}

function renderSettings() {
  const openWorkspace = vi.fn();
  const onOpenChange = vi.fn();
  const unused = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>
    <SettingsDialog checkout={checkout} open onOpenChange={onOpenChange} initialPage="customizations"
      openActivity={unused} openWorkspace={openWorkspace} openChanges={unused} />
  </QueryClientProvider>);
  return { openWorkspace, onOpenChange, user: userEvent.setup() };
}

afterEach(() => {
  cleanup();
  registrations.splice(0).forEach((remove) => remove());
  vi.resetAllMocks();
});

it("copies inventory metadata and opens only the selected local declaration through the editor", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "review_checkout_customizations") return inventory;
    if (command === "appearance_settings") return defaultAppearance;
    if (command === "open_checkout_item_in_shell") return;
    throw new Error(`Unexpected command ${command}`);
  });
  const { user, openWorkspace, onOpenChange } = renderSettings();
  const declaration = await screen.findByText("SessionStart");
  fireEvent.contextMenu(declaration);
  await user.click(screen.getByRole("menuitem", { name: "Copy declaration metadata" }));
  expect(JSON.parse(await navigator.clipboard.readText())).toEqual(hook);

  const instruction = screen.getByText(instructions.file.path);
  fireEvent.keyDown(instruction, { key: "F10", shiftKey: true });
  expect(screen.queryByRole("menuitem", { name: "Add saved instruction to conversation draft" })).toBeNull();
  await user.click(screen.getByRole("menuitem", { name: "Show instruction in File Explorer" }));
  expect(invoke).toHaveBeenCalledWith("open_checkout_item_in_shell", { checkoutId: checkout.checkoutId,
    relativePath: "docs/AGENTS.md", action: "reveal" });

  fireEvent.contextMenu(declaration);
  await user.click(screen.getByRole("menuitem", { name: "Open declaration in workbench" }));
  expect(onOpenChange).toHaveBeenCalledWith(false);
  expect(openWorkspace).toHaveBeenCalledExactlyOnceWith("ide", expect.any(String));
  expect(readSourceLineTarget(openWorkspace.mock.calls[0]?.[1])).toEqual({ path: ".claude/settings.json" });

  fireEvent.contextMenu(instruction);
  await user.click(screen.getByRole("menuitem", { name: "Open instruction in workbench" }));
  expect(openWorkspace).toHaveBeenLastCalledWith("knowledge", expect.any(String));
  expect(readNoteSearchTarget(openWorkspace.mock.calls[1]?.[1])).toMatchObject({ path: "docs/AGENTS.md" });
});

it("appends saved instructions and a compatible skill to the active checkout draft only", async () => {
  let saved = "Existing draft";
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "appearance_settings": return defaultAppearance;
      case "review_checkout_customizations": return inventory;
      case "composer_customizations": return [skill, external];
      case "conversation_draft": return saved;
      case "append_checkout_context_to_conversation": {
        if (!args || !("context" in args)) throw new Error("Missing draft context");
        saved += `\n\n${args.context}`;
        return;
      }
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  selectDraft({ ...scope, checkoutId: "other-checkout", conversationId: "other-chat" });
  const { user } = renderSettings();
  const instruction = await screen.findByText(instructions.file.path);
  expect(screen.queryByRole("button", { name: "Find compatible skills" })).toBeNull();
  act(() => { selectDraft(); });
  fireEvent.contextMenu(instruction);
  await user.click(screen.getByRole("menuitem", { name: "Add saved instruction to conversation draft" }));
  expect(await screen.findByText("Added to the draft for Release review.")).toBeTruthy();
  expect(saved).toContain(`Existing draft\n\nSaved instruction from docs\\AGENTS.md\n\n\`\`\`markdown\nUse the local fixture.\r\n\\${skillMarker}\r\n\`\`\``);
  expect(hasComposerSkills(saved)).toBe(false);

  expect(invoke).not.toHaveBeenCalledWith("composer_customizations", expect.anything());
  await user.click(screen.getByRole("button", { name: "Find compatible skills" }));
  const outsideSkill = await screen.findByText("plugin · External export");
  fireEvent.contextMenu(outsideSkill);
  expect(screen.queryByRole("menuitem", { name: "Open skill source in workbench" })).toBeNull();
  await user.keyboard("{Escape}");
  const localSkill = screen.getByText("check-links");
  fireEvent.contextMenu(localSkill);
  expect(screen.getByRole("menuitem", { name: "Open skill source in workbench" })).toBeTruthy();
  await user.click(screen.getByRole("menuitem", { name: "Add skill to conversation draft" }));
  await waitFor(() => expect(saved.endsWith(skillMarker)).toBe(true));
  expect(invoke).toHaveBeenCalledWith("composer_customizations", { conversationId: scope.conversationId,
    selection: scope.selection, effort: scope.effort, mode: scope.mode });
  expect(invoke).toHaveBeenCalledWith("append_checkout_context_to_conversation", { checkoutId: checkout.checkoutId,
    conversationId: scope.conversationId, context: skillMarker });

  fireEvent.contextMenu(localSkill);
  await user.click(screen.getByRole("menuitem", { name: "Add skill to conversation draft" }));
  expect(await screen.findByText(/This skill is already/)).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "append_checkout_context_to_conversation")).toHaveLength(2);
});

it("drops an in-flight skill selection when the active conversation changes", async () => {
  let finishRead!: (text: string) => void;
  const pendingDraft = new Promise<string>((resolve) => { finishRead = resolve; });
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "appearance_settings": return defaultAppearance;
      case "review_checkout_customizations": return inventory;
      case "composer_customizations": return [skill];
      case "conversation_draft": return pendingDraft;
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  selectDraft();
  const { user } = renderSettings();
  await user.click(await screen.findByRole("button", { name: "Find compatible skills" }));
  fireEvent.contextMenu(await screen.findByText("check-links"));
  await user.click(screen.getByRole("menuitem", { name: "Add skill to conversation draft" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("conversation_draft", { conversationId: scope.conversationId }));
  act(() => { selectDraft({ ...scope, conversationId: "new-active-conversation", title: "Other review" }); });
  finishRead("Prior draft");
  expect(await screen.findByText(/The active conversation or its draft changed/)).toBeTruthy();
  expect(invoke).not.toHaveBeenCalledWith("append_checkout_context_to_conversation", expect.anything());
});
