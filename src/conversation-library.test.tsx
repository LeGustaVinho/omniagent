// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "@/app";
import type { Conversation } from "@/bindings/Conversation";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { RunRecord } from "@/bindings/RunRecord";
import { RealConversations } from "@/desktop-dock";
import { defaultAppearance } from "@/settings-appearance";
import { useWorkspace } from "@/workspace-state";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
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
const archiveId = "older-chat";
const liveId = "live-chat";
const externalId = "external-chat";
const original = "Archive discovery";
const libraryTitle = "Archive review";
const finalTitle = "Archive release";
const draft = "Draft written before restart.";

function repository() {
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
  const drafts = new Map([[archiveId, draft]]);
  let activeCheckout = selected;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const input = (args ?? {}) as {
      checkoutId?: string; conversationId?: string; title?: string; query?: string; projectId?: string;
      workspace?: string; layout?: string; runId?: string; after?: string; text?: string;
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
          return (!input.projectId || input.projectId === summary.checkout.projectId) &&
            (!input.query || chat.title.toLowerCase().includes(input.query.toLowerCase())) ? [{ ...summary, conversation: chat }] : [];
        });
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
      case "conversation_reference_draft": return [];
      case "save_conversation_draft":
        drafts.set(input.conversationId!, input.text!);
        return;
      case "conversation_history":
        return { contextId: conversation?.contextId, messages: [{ messageId: "saved-user", revisionId: "saved-revision",
          role: "user", text: input.conversationId === archiveId ? "Saved conversation before provider shutdown."
            : input.conversationId === externalId ? "Saved transcript in the audit worktree." : "Saved live prompt." }] };
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
  const titles = within(library().getByRole("list")).getAllByRole("listitem").map((row) => row.querySelector("button")?.getAttribute("aria-label"));
  expect(titles).toEqual(["Live handoff", "Audit details", original]);
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

  await first.user.click(library().getByRole("button", { name: `Rename ${original}` }));
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

it("opens an all-project result in its own worktree from the actual application shell", async () => {
  repository();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><App /></QueryClientProvider>);
  const user = userEvent.setup();
  await screen.findByRole("region", { name: "Conversation library" });
  await user.selectOptions(library().getByRole("combobox", { name: "Project" }), "");
  await user.click(await library().findByRole("button", { name: "Audit details" }));
  expect(await screen.findByText("Saved transcript in the audit worktree.")).toBeTruthy();
  expect(await screen.findByRole("tab", { name: "Audit details" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Switch repository or worktree" }).textContent).toContain("Audit / audit-review");
  expect(invoke).toHaveBeenCalledWith("select_active_checkout", { checkoutId: other.checkoutId });
  expect(invoke).toHaveBeenCalledWith("checkout_conversation", { checkoutId: other.checkoutId, conversationId: externalId });
  expect(invoke).not.toHaveBeenCalledWith("checkout_conversation", { checkoutId: selected.checkoutId, conversationId: externalId });
});
