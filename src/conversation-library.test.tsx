// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "@/app";
import { activeCustomizationDraft } from "@/active-customization-draft";
import type { Conversation } from "@/bindings/Conversation";
import type { MessageRevision } from "@/bindings/MessageRevision";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { RunRecord } from "@/bindings/RunRecord";
import { RealConversations } from "@/desktop-dock";
import { defaultAppearance } from "@/settings-appearance";
import { useWorkspace } from "@/workspace-state";
import { RunInspectorNavigation } from "@/run-inspector-navigation";
import { takeTerminalReview } from "@/terminal-reviewed-input";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  isTauri: () => false,
  Channel: class { onmessage = () => {}; },
}));

beforeEach(() => {
  localStorage.clear();
  vi.stubGlobal("ResizeObserver", class {
    observe() {}
    unobserve() {}
    disconnect() {}
  });
});

afterEach(() => {
  cleanup();
  useWorkspace.getState().selectCheckout(null);
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});

const selected: ProjectCheckout = {
  projectId: "local", checkoutId: "main", repositoryRoot: "C:\\repos\\Launch", checkoutRoot: "C:\\repos\\Launch",
};
const other: ProjectCheckout = {
  projectId: "external", checkoutId: "external-review", repositoryRoot: "C:\\repos\\Audit", checkoutRoot: "C:\\worktrees\\audit-review",
};
const forkCheckout: ProjectCheckout = { ...selected, checkoutId: "new-fork-checkout", checkoutRoot: "C:\\worktrees\\launch-fork" };
const archiveId = "older-chat";
const liveId = "live-chat";
const externalId = "external-chat";
const original = "Archive discovery";
const libraryTitle = "Archive review";
const finalTitle = "Archive release";
const draft = "Draft written before restart.";

function repository(archiveMessages: MessageRevision[] = []) {
  const layouts = new Map<string, string>();
  const conversations = new Map<string, Conversation>([
    [archiveId, { id: archiveId, checkoutId: selected.checkoutId, contextId: "archived-context", title: original }],
    [liveId, { id: liveId, checkoutId: selected.checkoutId, contextId: "streaming-context", title: "Live handoff" }],
    [externalId, { id: externalId, checkoutId: other.checkoutId, contextId: "remote-context", title: "Audit details" }],
  ]);
  const summaries = [
    { id: liveId, checkout: selected, projectName: "Launch", agent: "codex", model: "gpt-test", status: "running", lastActivityAt: 1_700_000_001_000 },
    { id: externalId, checkout: other, projectName: "Audit", agent: "claude", model: "sonnet-test", status: "succeeded", lastActivityAt: 1_700_000_000_500 },
    { id: archiveId, checkout: selected, projectName: "Launch", agent: "codex", model: "gpt-old", status: "interrupted", lastActivityAt: 1_700_000_000_000 },
  ];
  type Section = { id: string; projectId: string; name: string };
  type Organization = { pinned: boolean; unread: boolean; archived: boolean; section: Section | null };
  const sections: Section[] = [{ id: "external-section", projectId: other.projectId, name: "Audit only" }];
  const organization = new Map<string, Organization>();
  const getOrganization = (id: string) => organization.get(id) ?? { pinned: false, unread: false, archived: false, section: null };
  const drafts = new Map([[archiveId, draft]]);
  let activeCheckout = selected;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const input = (args ?? {}) as {
      checkoutId?: string; conversationId?: string; title?: string; query?: string; projectId?: string;
      archived?: boolean; sectionId?: string | null;
      selection?: { conversationId: string; checkoutId: string; expectedSectionId: string | null }[];
      change?: { type: string; pinned?: boolean; unread?: boolean; archived?: boolean; sectionId?: string; name?: string;
        expectedName?: string; neighborId?: string; before?: boolean };
      workspace?: string; layout?: string; runId?: string; after?: string; text?: string; choice?: string;
    };
    const layoutKey = `${input.checkoutId}:${input.workspace}`;
    const conversation = conversations.get(input.conversationId ?? "");
    switch (command) {
      case "appearance_settings": return defaultAppearance;
      case "active_checkout": return activeCheckout;
      case "select_active_checkout":
        activeCheckout = input.checkoutId === other.checkoutId ? other : selected;
        return;
      case "run_snapshot": return { cursor: "0", runs: [], recentEvents: [] };
      case "subscribe_events": return `subscription-${input.checkoutId}`;
      case "unsubscribe_events": return;
      case "registered_checkouts": return [selected, other];
      case "workbench_layout": return layouts.get(layoutKey) ?? null;
      case "save_workbench_layout":
        layouts.set(layoutKey, input.layout!);
        return;
      case "conversation_library":
        return summaries.flatMap(({ id, ...summary }) => {
          const chat = conversations.get(id)!;
          const state = getOrganization(id);
          return (!input.projectId || input.projectId === summary.checkout.projectId) &&
            (input.archived ?? false) === state.archived && (!input.query || chat.title.toLowerCase().includes(input.query.toLowerCase()))
            ? [{ ...summary, conversation: chat, organization: state }] : [];
        });
      case "conversation_library_sections":
        return sections.filter(({ projectId }) => !input.projectId || projectId === input.projectId);
      case "organize_conversation_library_section": {
        const index = sections.findIndex((section) => section.projectId === input.projectId && section.id === input.sectionId);
        if (index < 0) throw new Error("Section does not belong to this project");
        const section = sections[index]!;
        const change = input.change!;
        if (change.type === "rename") {
          if (change.expectedName !== section.name) throw new Error("Section changed");
          if (sections.some((other) => other !== section && other.projectId === section.projectId && other.name.toLowerCase() === change.name?.toLowerCase())) {
            throw new Error("A section already exists in this project");
          }
          section.name = change.name!;
        } else {
          const ordered = sections.filter((item) => item.projectId === section.projectId);
          const neighbor = ordered[ordered.indexOf(section) + (change.before ? -1 : 1)];
          if (!neighbor || neighbor.id !== change.neighborId) throw new Error("Section order changed");
          sections[sections.indexOf(neighbor)] = section;
          sections[index] = neighbor;
        }
        return;
      }
      case "move_conversations_to_library_section": {
        const destination = sections.find(({ id, projectId }) => id === input.sectionId && projectId === input.projectId) ?? null;
        if (input.sectionId && !destination) throw new Error("Section does not belong to this project");
        const selection = input.selection!;
        for (const { conversationId, checkoutId, expectedSectionId } of selection) {
          const entry = summaries.find(({ id }) => id === conversationId);
          if (entry?.checkout.checkoutId !== checkoutId || entry.checkout.projectId !== input.projectId) throw new Error("Wrong worktree scope");
          if ((getOrganization(conversationId).section?.id ?? null) !== expectedSectionId) throw new Error("Changed membership");
        }
        let moved = 0;
        for (const { conversationId, expectedSectionId } of selection) {
          if (expectedSectionId === input.sectionId) continue;
          organization.set(conversationId, { ...getOrganization(conversationId), section: destination });
          moved++;
        }
        return moved;
      }
      case "export_conversation_markdown":
        if (!conversation || conversation.checkoutId !== input.checkoutId) throw new Error("Wrong export scope");
        return true;
      case "organize_conversation": {
        if (!conversation || conversation.checkoutId !== input.checkoutId) throw new Error("Wrong organization scope");
        const change = input.change!;
        const state = { ...getOrganization(conversation.id) };
        if (change.type === "read" && state.unread === change.unread) return false;
        if (change.type === "pin") state.pinned = change.pinned!;
        if (change.type === "read") state.unread = change.unread!;
        if (change.type === "archive") state.archived = change.archived!;
        if (change.type === "move") state.section = sections.find(({ id }) => id === change.sectionId) ?? null;
        if (change.type === "newSection") {
          const projectId = summaries.find(({ id }) => id === conversation.id)!.checkout.projectId;
          if (sections.some((item) => item.projectId === projectId && item.name.toLowerCase() === change.name?.toLowerCase())) {
            throw new Error("A section already exists in this project");
          }
          state.section = { id: `section-${sections.length + 1}`, projectId, name: change.name! };
          sections.push(state.section);
        }
        organization.set(conversation.id, state);
        return true;
      }
      case "checkout_conversation":
        if (!conversation || conversation.checkoutId !== input.checkoutId) throw new Error("Conversation does not belong to this checkout");
        return conversation;
      case "rename_conversation": {
        if (!conversation || conversation.checkoutId !== input.checkoutId) throw new Error("Wrong rename scope");
        const updated = { ...conversation, title: input.title! };
        conversations.set(updated.id, updated);
        return updated;
      }
      case "conversation_draft": return drafts.get(input.conversationId!) ?? "";
      case "conversation_fork_checkout_settings":
        return { projectDefault: "reuse_current_checkout", conversationOverride: null, effective: "reuse_current_checkout" };
      case "conversation_queue_history_policy": return { projectDefault: "review", conversationOverride: null, effective: "review" };
      case "preview_conversation_fork_checkout":
        return { previewId: "fork-preview", choice: input.choice ?? "reuse_current_checkout", projectId: selected.projectId,
          checkoutId: selected.checkoutId, checkoutRoot: selected.checkoutRoot, repositoryRoot: selected.repositoryRoot,
          baseCommit: null, dirtyPaths: [], selectedPaths: [],
          targetCheckoutRoot: input.choice === "create_separate_worktree" ? forkCheckout.checkoutRoot : null, targetBranch: "codex/launch-fork" };
      case "fork_conversation_with_checkout":
        return { id: "forked-chat", checkoutId: forkCheckout.checkoutId, contextId: "fork-context", title: `${original} (fork)` };
      case "conversation_reference_draft": return [];
      case "conversation_history_deletion_diffs": return [];
      case "conversation_attachment_draft": return { revision: null, files: [] };
      case "save_conversation_draft":
        drafts.set(input.conversationId!, input.text!);
        return;
      case "conversation_history":
        return { contextId: conversation?.contextId, messages: [{ messageId: "saved-user", revisionId: "saved-revision",
          role: "user", text: input.conversationId === archiveId ? "Saved conversation before provider shutdown."
            : input.conversationId === externalId ? "Saved transcript in the audit worktree." : "Saved live prompt." },
          ...(input.conversationId === archiveId ? archiveMessages : [])] };
      case "latest_conversation_run":
        if (input.conversationId === externalId) return null;
        return { id: input.conversationId === liveId ? "existing-run" : "previous-run", projectId: selected.projectId,
          checkoutId: selected.checkoutId, turnId: "saved-turn",
          status: input.conversationId === liveId ? "running" : "interrupted" } satisfies RunRecord;
      case "conversation_events":
        return { cursor: "1", more: false, events: input.after !== "0" ? [] : [{
          data: input.runId === "existing-run" ? { type: "assistant_delta", text: "Already running before the tab opened." }
            : { type: "error", message: "Provider session unavailable." },
          location: { itemId: "output", parentItemId: null },
        }] };
      case "latest_conversation_job": return null;
      case "composer_suggestion_mode": return "off";
      case "list_checkout_folders": return ["scripts"];
      case "create_checkout_item": return "scripts/code.ps1";
      case "conversation_models":
      case "pending_conversation_instructions":
      case "pending_conversation_inputs":
      case "saved_conversation_questions": return [];
      default: throw new Error(`Unexpected IPC command ${command}`);
    }
  });
  return { layouts };
}

function mount(onOpenOtherCheckout = vi.fn(async () => {})) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const ui = render(<QueryClientProvider client={client}>
    <RealConversations checkout={selected} sidebar compact={false} onSidebarChange={vi.fn()} onOpenOtherCheckout={onOpenOtherCheckout} />
  </QueryClientProvider>);
  return { ...ui, user: userEvent.setup(), onOpenOtherCheckout };
}

function library() { return within(screen.getByRole("region", { name: "Conversation library" })); }

async function rename(user: ReturnType<typeof userEvent.setup>, title: string) {
  const form = within(screen.getByRole("form", { name: "Rename conversation" }));
  const input = form.getByRole("textbox", { name: "Conversation title" });
  await user.clear(input);
  await user.type(input, title);
  await user.click(form.getByRole("button", { name: "Save" }));
}

it("shows newest activity first, filters by project and title, and reconnects to the existing live run by conversation ID", async () => {
  repository();
  const { user, onOpenOtherCheckout } = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  await library().findByRole("button", { name: "Live handoff" });
  const project = library().getByRole("combobox", { name: "Project" });
  expect((project as HTMLSelectElement).value).toBe(selected.projectId);
  expect(library().queryByRole("button", { name: "Audit details" })).toBeNull();

  await user.selectOptions(project, "");
  await library().findByRole("button", { name: "Audit details" });
  const launch = within(library().getByRole("region", { name: "Launch · Conversations" }));
  const titles = launch.getAllByRole("listitem").map((row) => row.querySelector("button")?.getAttribute("aria-label"));
  expect(titles).toEqual(["Live handoff", original]);
  expect(within(library().getByRole("region", { name: "Audit · Conversations" })).getByRole("button", { name: "Audit details" })).toBeTruthy();
  expect(library().getByText("Launch · Codex · gpt-test · running")).toBeTruthy();
  expect(library().getByText("Audit · Claude Code · sonnet-test · succeeded")).toBeTruthy();
  await user.selectOptions(project, other.projectId);
  await library().findByRole("button", { name: "Audit details" });
  expect(library().queryByRole("button", { name: "Live handoff" })).toBeNull();
  await user.type(library().getByRole("searchbox"), "Audit");
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("conversation_library", { query: "Audit", projectId: other.projectId }));
  await user.click(await library().findByRole("button", { name: "Audit details" }));
  expect(onOpenOtherCheckout).toHaveBeenCalledExactlyOnceWith(other, externalId);

  await user.clear(library().getByRole("searchbox"));
  await user.selectOptions(project, selected.projectId);
  await user.click(await library().findByRole("button", { name: "Live handoff" }));
  expect(await screen.findByText("Already running before the tab opened.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Stop" })).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("conversation_events", { conversationId: liveId, runId: "existing-run", after: "0" });
  expect(invoke).not.toHaveBeenCalledWith("start_conversation_turn", expect.anything());
});

it("opens a new conversation beside the active panel only within the selected worktree", async () => {
  const { layouts } = repository();
  const { user, onOpenOtherCheckout } = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  await user.click(await library().findByRole("button", { name: original }));
  const referenceTab = await screen.findByRole("tab", { name: original });
  const openBeside = async (title: string) => {
    fireEvent.contextMenu(await library().findByRole("button", { name: title }));
    await user.click(await screen.findByRole("menuitem", { name: "Open conversation to the side" }));
  };
  await openBeside("Live handoff");
  const liveTab = await screen.findByRole("tab", { name: "Live handoff" });
  expect(liveTab.closest(".dv-groupview")).not.toBe(referenceTab.closest(".dv-groupview"));
  await waitFor(() => expect(layouts.get(`${selected.checkoutId}:conversations`)).toContain(liveId));
  expect(invoke).toHaveBeenCalledWith("checkout_conversation", { checkoutId: selected.checkoutId, conversationId: liveId });

  await user.click(library().getByRole("button", { name: original }));
  await openBeside("Live handoff");
  await waitFor(() => expect(library().getByRole("button", { name: "Live handoff" }).getAttribute("aria-current")).toBe("page"));
  expect(screen.getAllByRole("tab", { name: "Live handoff" })).toEqual([liveTab]);
  expect(liveTab.closest(".dv-groupview")).not.toBe(referenceTab.closest(".dv-groupview"));

  await user.selectOptions(library().getByRole("combobox", { name: "Project" }), "");
  fireEvent.contextMenu(await library().findByRole("button", { name: "Audit details" }));
  const menu = within(await screen.findByRole("menu"));
  expect(menu.queryByRole("menuitem", { name: "Open conversation to the side" })).toBeNull();
  await user.click(menu.getByRole("menuitem", { name: "Open conversation" }));
  expect(onOpenOtherCheckout).toHaveBeenCalledExactlyOnceWith(other, externalId);
});

it("moves an already open conversation to the side with the same mounted composer and draft", async () => {
  repository();
  const { user } = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  await user.click(await library().findByRole("button", { name: original }));
  const input = await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" });
  await waitFor(() => expect(activeCustomizationDraft(selected.checkoutId)?.conversationId).toBe(archiveId));
  const edited = `${draft}\nRetain this unsent edit across a split.`;
  fireEvent.change(input, { target: { value: edited } });
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("save_conversation_draft", { conversationId: archiveId, text: edited }));
  await user.click(library().getByRole("button", { name: "Live handoff" }));
  const companion = await screen.findByRole("tab", { name: "Live handoff" });
  await waitFor(() => expect(activeCustomizationDraft(selected.checkoutId)).toBeUndefined());
  const originalTab = () => screen.getByRole("tab", { name: original });
  expect(originalTab().closest(".dv-groupview")).toBe(companion.closest(".dv-groupview"));

  const row = library().getByRole("button", { name: original });
  row.focus();
  await user.keyboard("{Shift>}{F10}{/Shift}");
  await user.click(await screen.findByRole("menuitem", { name: "Open conversation to the side" }));
  await waitFor(() => expect(originalTab().closest(".dv-groupview")).not.toBe(companion.closest(".dv-groupview")));
  await waitFor(() => expect(screen.getAllByRole("textbox", { name: "Message draft" })).toContain(input));
  await waitFor(() => expect(activeCustomizationDraft(selected.checkoutId)?.conversationId).toBe(archiveId));
  expect(input.value).toBe(edited);
  expect(screen.getAllByRole("tab", { name: original })).toHaveLength(1);
  expect(invoke).not.toHaveBeenCalledWith("start_conversation_turn", expect.anything());
});

it("persists library and tab renames, restores older history and draft, and shares the saved title with the reference picker", async () => {
  const { layouts } = repository();
  const first = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  await first.user.type(library().getByRole("searchbox"), "Archive");
  await first.user.click(await library().findByRole("button", { name: original }));
  expect(await screen.findByText("Saved conversation before provider shutdown.")).toBeTruthy();
  expect(await screen.findByText("Provider session unavailable.")).toBeTruthy();
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" }).value).toBe(draft);
  const layoutKey = `${selected.checkoutId}:conversations`;
  await waitFor(() => expect(layouts.get(layoutKey)).toContain(original));
  const oldLayout = layouts.get(layoutKey)!;

  library().getByRole("button", { name: original }).focus();
  await first.user.keyboard("{Shift>}{F10}{/Shift}");
  await first.user.click(await screen.findByRole("menuitem", { name: "Rename" }));
  await rename(first.user, `  ${libraryTitle}  `);
  expect(await screen.findByRole("tab", { name: libraryTitle })).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("rename_conversation", { checkoutId: selected.checkoutId, conversationId: archiveId, title: libraryTitle });
  await first.user.click(screen.getByRole("button", { name: "Rename conversation" }));
  await rename(first.user, finalTitle);
  expect(await library().findByRole("button", { name: finalTitle })).toBeTruthy();
  expect(await screen.findByRole("tab", { name: finalTitle })).toBeTruthy();
  first.unmount();
  localStorage.clear();
  layouts.set(layoutKey, oldLayout);

  const reopened = mount();
  expect(await screen.findByRole("tab", { name: finalTitle })).toBeTruthy();
  expect(await screen.findByText("Saved conversation before provider shutdown.")).toBeTruthy();
  expect((await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" })).value).toBe(draft);
  await reopened.user.click(await library().findByRole("button", { name: "Live handoff" }));
  await reopened.user.selectOptions(library().getByRole("combobox", { name: "Project" }), "");
  await library().findByRole("button", { name: "Audit details" });
  const live = within((await screen.findByRole("heading", { name: "Live handoff" })).closest("section")!);
  await reopened.user.click(live.getByRole("button", { name: "Find conversation references" }));
  const references = within(await screen.findByRole("dialog", { name: "Conversation references" }));
  const referenceSearch = references.getByRole("searchbox");
  await references.findByRole("button", { name: `Open source ${finalTitle}` });
  expect(references.queryByRole("button", { name: "Open source Audit details" })).toBeNull();
  await reopened.user.type(referenceSearch, "Audit");
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("conversation_library", { query: "Audit", projectId: selected.projectId }));
  expect(await references.findByText("No matching conversations.")).toBeTruthy();
  await reopened.user.clear(referenceSearch);
  await reopened.user.type(referenceSearch, "Archive");
  expect(await references.findByRole("button", { name: `Open source ${finalTitle}` })).toBeTruthy();
  expect(references.getByText("Launch · Codex · gpt-old · interrupted")).toBeTruthy();
  await reopened.user.click(references.getByRole("button", { name: `Open source ${finalTitle}` }));
  await waitFor(() => expect(library().getByRole("button", { name: finalTitle }).getAttribute("aria-current")).toBe("page"));

  await reopened.user.click(library().getByRole("button", { name: "Live handoff" }));
  await reopened.user.type(library().getByRole("searchbox"), "Archive release");
  await reopened.user.click(await library().findByRole("button", { name: finalTitle }));
  await waitFor(() => expect(library().getByRole("button", { name: finalTitle }).getAttribute("aria-current")).toBe("page"));
  expect(screen.getAllByRole("tab", { name: finalTitle })).toHaveLength(1);
  expect(invoke).not.toHaveBeenCalledWith("start_conversation_turn", expect.anything());
});

it("offers the shared context menu on a live library row and copies that row's title", async () => {
  repository();
  const { user } = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  const row = await library().findByRole("button", { name: original });
  fireEvent.contextMenu(row);
  const menu = await screen.findByRole("menu");
  expect(within(menu).getByRole("menuitem", { name: "Rename" })).toBeTruthy();
  expect(within(menu).getByRole("menuitem", { name: "Archive" })).toBeTruthy();
  const copy = within(menu).getByRole("menuitem", { name: "Copy" });
  copy.focus();
  await user.keyboard("{ArrowRight}");
  await user.click(await screen.findByRole("menuitem", { name: "Title" }));
  expect(await screen.findByText("Title copied.")).toBeTruthy();
  expect(await navigator.clipboard.readText()).toBe(original);
});

it("copies highlighted code and appends its own fenced block to the current draft without sending", async () => {
  const source = 'const marker = "```";';
  const block = `\`\`\`\`typescript\n${source}\n\`\`\`\``;
  repository([{ messageId: "code", revisionId: "code-revision", role: "assistant", text: `Run this in a scratch file.\n\n${block}\n\nThen inspect its output.` }]);
  const { user } = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  await user.click(await library().findByRole("button", { name: original }));
  const code = await screen.findByLabelText("typescript code");
  fireEvent.contextMenu(code);
  expect(screen.queryByRole("menuitem", { name: "Copy message as Markdown" })).toBeNull();
  await user.click(await screen.findByRole("menuitem", { name: "Copy code" }));
  expect(await navigator.clipboard.readText()).toBe(`${source}\n`);

  code.focus();
  await user.keyboard("{Shift>}{F10}{/Shift}");
  await user.click(await screen.findByRole("menuitem", { name: "Add code block to draft" }));
  const expected = `${draft}\n\n${block}\n\n`;
  await waitFor(() => expect(screen.getByRole("textbox", { name: "Message draft" })).toHaveProperty("value", expected));
  expect(invoke).toHaveBeenCalledWith("save_conversation_draft", { conversationId: archiveId, text: expected });

  const paragraph = screen.getByText("Run this in a scratch file.");
  const range = document.createRange();
  const snippet = "Run this";
  range.setStart(paragraph.firstChild!, 0);
  range.setEnd(paragraph.firstChild!, snippet.length);
  const selection = window.getSelection()!;
  selection.removeAllRanges();
  selection.addRange(range);
  fireEvent.contextMenu(paragraph);
  await user.click(await screen.findByRole("menuitem", { name: "Quote selected text in draft" }));
  const quoted = `${expected}> ${snippet}\n\n`;
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("save_conversation_draft", { conversationId: archiveId, text: quoted }));
  expect(invoke).not.toHaveBeenCalledWith("start_conversation_turn", expect.anything());
});

it("confirms a checkout file save and stages the same clicked block for the terminal without running it", async () => {
  const source = '$value = "café";\r\n\r\nWrite-Output $value\r\n';
  const nested = source.split("\r\n").slice(0, -1).map((line) => `> ${line}\r\n`).join("");
  repository([{ messageId: "review-code", revisionId: "review-code-revision", role: "assistant", text:
    `\x60\x60\x60powershell\nGet-Date\n\x60\x60\x60\n\n> \x60\x60\x60powershell\r\n${nested}> \x60\x60\x60` }]);
  useWorkspace.getState().selectCheckout(selected);
  const openWorkspace = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><RunInspectorNavigation.Provider value={{ openActivity: vi.fn(), openWorkspace }}>
    <RealConversations checkout={selected} sidebar compact={false} onSidebarChange={vi.fn()} />
  </RunInspectorNavigation.Provider></QueryClientProvider>);
  const user = userEvent.setup();
  await screen.findByRole("region", { name: "Conversation library" });
  await user.click(await library().findByRole("button", { name: original }));
  const code = (await screen.findAllByLabelText("powershell code"))[1]!;
  fireEvent.contextMenu(code);
  await user.click(await screen.findByRole("menuitem", { name: "Save code block as file…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Save code block as file" }));
  expect(dialog.getByLabelText("Code to save").textContent).toBe(source);
  expect(dialog.getByRole("textbox", { name: "File name" })).toHaveProperty("value", "code.ps1");
  await dialog.findByRole("option", { name: "scripts" });
  await user.selectOptions(dialog.getByRole("combobox", { name: "Destination folder" }), "scripts");
  expect(invoke).not.toHaveBeenCalledWith("create_checkout_item", expect.anything());
  await user.click(dialog.getByRole("button", { name: "Save new file" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("create_checkout_item", {
    checkoutId: selected.checkoutId, relativeDirectory: "scripts", name: "code.ps1", directory: false, contents: source,
  }));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Save code block as file" })).toBeNull());
  expect(openWorkspace).toHaveBeenCalledWith("ide", expect.stringContaining("scripts/code.ps1"));

  fireEvent.contextMenu((await screen.findAllByLabelText("powershell code"))[1]!);
  await user.click(await screen.findByRole("menuitem", { name: "Insert in terminal for review" }));
  const [workspace, target] = openWorkspace.mock.lastCall as [string, string];
  expect(workspace).toBe("ide");
  expect(target).not.toContain("Write-Output");
  expect(takeTerminalReview(target, selected.checkoutId)?.text).toBe(source);
  expect(takeTerminalReview(target, selected.checkoutId)).toBeUndefined();
  expect(invoke).not.toHaveBeenCalledWith("start_manual_terminal", expect.anything());
  expect(invoke).not.toHaveBeenCalledWith("write_manual_terminal", expect.anything());
});

it("reviews a library fork against the clicked conversation before creating it", async () => {
  repository();
  const { user, onOpenOtherCheckout } = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  fireEvent.contextMenu(await library().findByRole("button", { name: original }));
  await user.click(await screen.findByRole("menuitem", { name: "Create fork…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Conversation history" }));
  await waitFor(() => expect(dialog.getByRole("button", { name: "Create fork" })).not.toHaveProperty("disabled", true));
  expect(dialog.getByRole("textbox", { name: "Fork title" })).toHaveProperty("value", `${original} (fork)`);
  expect(invoke).toHaveBeenCalledWith("preview_conversation_fork_checkout", {
    conversationId: archiveId, expectedContextId: "archived-context", choice: null, selectedPaths: [],
  });
  expect(invoke).not.toHaveBeenCalledWith("fork_conversation_with_checkout", expect.anything());
  await user.selectOptions(dialog.getByRole("combobox", { name: "Fork checkout" }), "create_separate_worktree");
  await dialog.findByText(`${forkCheckout.checkoutRoot} · codex/launch-fork`);
  await user.click(dialog.getByRole("button", { name: "Create fork" }));
  await waitFor(() => expect(onOpenOtherCheckout).toHaveBeenCalledExactlyOnceWith(forkCheckout, "forked-chat"));
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "fork_conversation_with_checkout")).toEqual([
    ["fork_conversation_with_checkout", { request: {
      conversationId: archiveId, expectedContextId: "archived-context", sourceContextId: null, throughRevision: null,
      title: `${original} (fork)`, editedText: null, checkoutPreviewId: "fork-preview",
    } }],
  ]);
});

it("organizes conversations through the shared menu and reloads persisted sections, unread and archived state", async () => {
  repository();
  const first = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  async function menu(title: string) {
    fireEvent.contextMenu(await library().findByRole("button", { name: title }));
    return within(await screen.findByRole("menu"));
  }

  await first.user.selectOptions(await library().findByRole("combobox", { name: "Project" }), "");
  const section = (await menu(original)).getByRole("menuitem", { name: "Section" });
  section.focus();
  await first.user.keyboard("{ArrowRight}");
  expect(screen.queryByRole("menuitem", { name: "Audit only" })).toBeNull();
  await first.user.click(await screen.findByRole("menuitem", { name: "New section…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Create section" }));
  await first.user.type(dialog.getByRole("textbox", { name: "Section name" }), "Release notes");
  await first.user.click(dialog.getByRole("button", { name: "Save" }));
  await screen.findByRole("region", { name: "Conversation library" });
  expect(await library().findByRole("region", { name: "Launch · Release notes" })).toBeTruthy();

  await first.user.click((await menu(original)).getByRole("menuitem", { name: "Pin" }));
  expect(await library().findByRole("region", { name: "Pinned" })).toBeTruthy();
  await first.user.click((await menu(original)).getByRole("menuitem", { name: "Mark as unread" }));
  await waitFor(() => expect(library().getByRole("button", { name: original }).getAttribute("aria-description")).toBe("Unread"));
  await first.user.click((await menu(original)).getByRole("menuitem", { name: "Archive" }));
  await waitFor(() => expect(library().queryByRole("button", { name: original })).toBeNull());
  first.unmount();

  const reopened = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  await reopened.user.click(await library().findByRole("button", { name: "Archived" }));
  const archivedRow = await within(await library().findByRole("region", { name: "Release notes" })).findByRole("button", { name: original });
  expect(archivedRow.getAttribute("aria-description")).toBe("Unread");
  await reopened.user.click((await menu(original)).getByRole("menuitem", { name: "Unarchive" }));
  await reopened.user.click(library().getByRole("button", { name: "Archived" }));
  expect(await library().findByRole("region", { name: "Pinned" })).toBeTruthy();
  await reopened.user.click((await menu(original)).getByRole("menuitem", { name: "Unpin" }));
  expect(await library().findByRole("region", { name: "Release notes" })).toBeTruthy();
  await reopened.user.click(library().getByRole("button", { name: original }));
  await waitFor(() => expect(library().getByRole("button", { name: original }).getAttribute("aria-description")).toBeNull());
});

it("renames and reorders a saved section, including empty sections, without changing membership", async () => {
  repository();
  const first = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  for (const name of ["Zulu", "Alpha"]) {
    fireEvent.contextMenu(await library().findByRole("button", { name: original }));
    const submenu = await screen.findByRole("menuitem", { name: "Section" });
    submenu.focus();
    await first.user.keyboard("{ArrowRight}");
    await first.user.click(await screen.findByRole("menuitem", { name: "New section…" }));
    const dialog = within(await screen.findByRole("dialog", { name: "Create section" }));
    await first.user.type(dialog.getByRole("textbox", { name: "Section name" }), name);
    await first.user.click(dialog.getByRole("button", { name: "Save" }));
    await screen.findByRole("region", { name });
  }
  const order = () => library().getAllByRole("heading", { level: 3 }).map((heading) => heading.textContent);
  expect(order()).toEqual(["Conversations", "Zulu", "Alpha"]);
  expect(within(library().getByRole("region", { name: "Zulu" })).getByText("No conversations in this section.")).toBeTruthy();

  const actions = library().getByRole("button", { name: "Actions for Alpha section" });
  actions.focus();
  await first.user.keyboard("{Shift>}{F10}{/Shift}");
  await first.user.click(await screen.findByRole("menuitem", { name: "Rename section…" }));
  const rename = within(await screen.findByRole("dialog", { name: "Rename section" }));
  const name = rename.getByRole("textbox", { name: "Section name" });
  await first.user.clear(name);
  await first.user.type(name, "Beta");
  await first.user.click(rename.getByRole("button", { name: "Save" }));
  const beta = await screen.findByRole("heading", { name: "Beta" });
  expect(order()).toEqual(["Conversations", "Zulu", "Beta"]);
  expect(invoke).toHaveBeenCalledWith("organize_conversation_library_section", {
    projectId: selected.projectId, sectionId: "section-3", change: { type: "rename", expectedName: "Alpha", name: "Beta" },
  });

  fireEvent.contextMenu(beta);
  expect((await screen.findByRole("menuitem", { name: "Move section down" })).hasAttribute("data-disabled")).toBe(true);
  await first.user.click(screen.getByRole("menuitem", { name: "Move section up" }));
  await waitFor(() => expect(order()).toEqual(["Conversations", "Beta", "Zulu"]));
  expect(invoke).toHaveBeenCalledWith("organize_conversation_library_section", {
    projectId: selected.projectId, sectionId: "section-3", change: { type: "move", neighborId: "section-2", before: true },
  });
  first.unmount();
  mount();
  await screen.findByRole("region", { name: "Beta" });
  expect(order()).toEqual(["Conversations", "Beta", "Zulu"]);
  expect(within(library().getByRole("region", { name: "Beta" })).getByRole("button", { name: original })).toBeTruthy();
});

it("moves a reviewed selection in one project from row and section context menus", async () => {
  repository();
  const { user } = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  await user.selectOptions(library().getByRole("combobox", { name: "Project" }), "");
  fireEvent.contextMenu(await library().findByRole("button", { name: original }));
  const submenu = await screen.findByRole("menuitem", { name: "Section" });
  submenu.focus();
  await user.keyboard("{ArrowRight}");
  await user.click(await screen.findByRole("menuitem", { name: "New section…" }));
  const create = within(await screen.findByRole("dialog", { name: "Create section" }));
  await user.type(create.getByRole("textbox", { name: "Section name" }), "Triage");
  await user.click(create.getByRole("button", { name: "Save" }));
  await screen.findByRole("region", { name: "Launch · Triage" });

  fireEvent.contextMenu(library().getByRole("button", { name: "Live handoff" }));
  await user.click(await screen.findByRole("menuitem", { name: "Select conversations to move…" }));
  expect((library().getByRole("checkbox", { name: "Select Live handoff" }) as HTMLInputElement).checked).toBe(true);
  expect(library().queryByRole("checkbox", { name: "Select Audit details" })).toBeNull();
  await user.click(library().getByRole("checkbox", { name: `Select ${original}` }));
  await user.click(library().getByRole("button", { name: "Move selected conversations…" }));
  const review = within(await screen.findByRole("dialog", { name: "Move selected conversations" }));
  expect(review.getByText("2 selected")).toBeTruthy();
  expect(review.getByText(original)).toBeTruthy();
  expect(review.getByText("Live handoff")).toBeTruthy();
  expect(review.queryByRole("option", { name: "Audit only" })).toBeNull();
  expect(invoke).not.toHaveBeenCalledWith("move_conversations_to_library_section", expect.anything());
  await user.selectOptions(review.getByRole("combobox", { name: "Destination section" }), "section-2");
  await user.click(review.getByRole("button", { name: "Move" }));
  const inSection = within(await screen.findByRole("region", { name: "Launch · Triage" }));
  expect(inSection.getByRole("button", { name: "Live handoff" })).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("move_conversations_to_library_section", {
    projectId: selected.projectId, sectionId: "section-2", selection: [
      { checkoutId: selected.checkoutId, conversationId: liveId, expectedSectionId: null },
      { checkoutId: selected.checkoutId, conversationId: archiveId, expectedSectionId: "section-2" },
    ],
  });

  fireEvent.contextMenu(inSection.getByRole("heading", { name: "Launch · Triage" }));
  await user.click(await screen.findByRole("menuitem", { name: "Select visible conversations to move…" }));
  expect(library().getByText("2 selected")).toBeTruthy();
  await user.click(library().getByRole("button", { name: "Move selected conversations…" }));
  const remove = within(await screen.findByRole("dialog", { name: "Move selected conversations" }));
  await user.selectOptions(remove.getByRole("combobox", { name: "Destination section" }), "");
  await user.click(remove.getByRole("button", { name: "Move" }));
  expect(await screen.findByText("Moved 2 conversations.")).toBeTruthy();
  expect(within(screen.getByRole("region", { name: "Launch · Triage" })).getByText("No conversations in this section.")).toBeTruthy();
  expect(invoke).not.toHaveBeenCalledWith("start_conversation_turn", expect.anything());
});

it("exports the clicked conversation using its actual checkout, including from Archived", async () => {
  repository();
  const { user, onOpenOtherCheckout } = mount();
  await screen.findByRole("region", { name: "Conversation library" });
  await user.selectOptions(await library().findByRole("combobox", { name: "Project" }), "");
  async function choose(title: string, action: string) {
    fireEvent.contextMenu(await library().findByRole("button", { name: title }));
    await user.click(await screen.findByRole("menuitem", { name: action }));
  }
  await choose("Audit details", "Export conversation as Markdown…");
  expect(invoke).toHaveBeenCalledWith("export_conversation_markdown", { checkoutId: other.checkoutId, conversationId: externalId });
  expect(await library().findByText("Conversation exported as Markdown.")).toBeTruthy();
  await choose("Audit details", "Archive");
  await user.click(library().getByRole("button", { name: "Archived" }));
  await choose("Audit details", "Export conversation as Markdown…");
  await waitFor(() => expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "export_conversation_markdown")).toHaveLength(2));
  expect(onOpenOtherCheckout).not.toHaveBeenCalled();
});

it("opens an all-project result in its own worktree from the actual application shell", async () => {
  repository();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><App /></QueryClientProvider>);
  const user = userEvent.setup();
  await screen.findByRole("region", { name: "Conversation library" }, { timeout: 8_000 });
  await user.selectOptions(library().getByRole("combobox", { name: "Project" }), "");
  await user.click(await library().findByRole("button", { name: "Audit details" }));
  expect(await screen.findByText("Saved transcript in the audit worktree.")).toBeTruthy();
  expect(await screen.findByRole("tab", { name: "Audit details" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Switch repository or worktree" }).textContent).toContain("Audit / audit-review");
  expect(invoke).toHaveBeenCalledWith("select_active_checkout", { checkoutId: other.checkoutId });
  expect(invoke).toHaveBeenCalledWith("checkout_conversation", { checkoutId: other.checkoutId, conversationId: externalId });
  expect(invoke).not.toHaveBeenCalledWith("checkout_conversation", { checkoutId: selected.checkoutId, conversationId: externalId });
});

it("opens saved conversations from the compact library and returns space to the transcript", async () => {
  repository();
  function CompactWorkspace() {
    const [sidebar, setSidebar] = useState(false);
    return <RealConversations checkout={selected} compact sidebar={sidebar} onSidebarChange={setSidebar} />;
  }
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><CompactWorkspace /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Show conversations" }));
  const drawer = screen.getByRole("dialog", { name: "Conversation navigation" });
  const search = within(drawer).getByRole("searchbox", { name: "Search conversation titles" });
  await waitFor(() => expect(document.activeElement).toBe(search));
  await user.type(search, "Archive");
  await user.click(await within(drawer).findByRole("button", { name: original }));
  expect(await screen.findByText("Saved conversation before provider shutdown.")).toBeTruthy();
  expect(screen.queryByRole("dialog", { name: "Conversation navigation" })).toBeNull();
  expect((await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" })).value).toBe(draft);
});
