// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, renderHook, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps, PropsWithChildren } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { composerCompletion, replaceComposerCompletion } from "@/composer-completion";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ConversationLibraryNavigation } from "@/conversation-library";
import { ConversationPanel } from "@/conversation-panels";
import { pipelineActionMarker, readPipelineActionMarker } from "@/conversation-customization-picker";
import { usePipelineComposerCustomizations } from "@/pipeline-composer-customizations";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => undefined) }));
beforeEach(() => vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} }));
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.resetAllMocks(); });

it("completes the whole token in the middle while leaving paths, URLs, email and Markdown code literal", () => {
  const text = "Read @Releas before approving.";
  const cursor = "Read @Rel".length;
  const token = composerCompletion(text, cursor)!;
  expect(token).toEqual({ trigger: "@", query: "Rel", start: 5, end: 12 });
  expect(replaceComposerCompletion(text, token, "@Release checklist")).toEqual({
    text: "Read @Release checklist before approving.", cursor: 24,
  });
  const slash = "Review /format then rerun.";
  expect(composerCompletion(slash, "Review /fo".length)).toEqual({ trigger: "/", query: "fo", start: 7, end: 14 });
  for (const [literal, caret] of [
    ["Use https://domain.example/@name", "Use https://domain.example/@name"],
    ["mail writer@repo", "mail writer@repo"],
    ["Open /src/tools", "Open /src"],
    ["Open @pkg/name", "Open @pkg"],
    ["Type `@literal`", "Type `@literal"],
    ["```ts\n /format", "```ts\n /format"],
    ["~~~\n@literal", "~~~\n@literal"],
    ["    /literal", "    /literal"],
    ["Escape \\@value", "Escape \\@value"],
  ] as const) expect(composerCompletion(literal, caret.length)).toBeUndefined();
  expect(composerCompletion("```\n/a\n```\n/bo", 14)).toMatchObject({ trigger: "/", query: "bo" });
});

it("keeps the exact stage ID and transport action in a restorable draft marker", () => {
  const action = { nodeId: "review:é", marker: `@command:${"1".repeat(64)}:${"a".repeat(64)}` };
  expect(readPipelineActionMarker(pipelineActionMarker(action.nodeId, action.marker))).toEqual(action);
});

it("scopes slash results to eligible stages and revalidates the targeted action after a profile change", async () => {
  const selection: ModelSelection = { provider: "claude", modelId: "haiku", profileId: "claude-profile" };
  const marker = `@command:${"1".repeat(64)}:${"a".repeat(64)}`;
  const action = { marker, route: "native_command", availability: "provider",
    skill: { provider: "claude", name: "format", path: "", namespace: null, revision: "pinned" } };
  vi.mocked(invoke).mockImplementation(async (_command, raw) => {
    const args = raw as { selection: ModelSelection; effort: string };
    expect(args.effort).toBe("low");
    return args.selection.profileId === selection.profileId ? [action] : [];
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const agents = [{ nodeId: "prepare", label: "Prepare" }, { nodeId: "review", label: "Review" },
    { nodeId: "loop", label: "Review each unit", actionEligible: false }];
  const { result, rerender } = renderHook(({ text, model }) => usePipelineComposerCustomizations({
    conversationId: "chat", agents, selection: model, text, effort: "low", query: "for", active: true, enabled: true,
  }), { initialProps: { text: "Please /for this release", model: selection },
    wrapper: ({ children }: PropsWithChildren) => <QueryClientProvider client={client}>{children}</QueryClientProvider> });
  await waitFor(() => expect(result.current.options.map(({ agent }) => agent.nodeId)).toEqual(["prepare", "review"]));
  expect(invoke).toHaveBeenCalledOnce();
  const token = pipelineActionMarker("review", marker);
  const text = `Please ${token} format this release`;
  rerender({ text, model: selection });
  expect(result.current.submission).toEqual({ text: "Please  format this release", action: { nodeId: "review", marker }, blocker: undefined });
  rerender({ text, model: { ...selection, profileId: "changed-profile" } });
  await waitFor(() => expect(result.current.submission.blocker).toMatch(/unavailable for this stage/));
});

it("pins an exact same-project conversation at the caret and restores and removes its saved reference", async () => {
  let saved = "Read @Rel before approving.";
  const destination = { id: "current", title: "Current", checkoutId: "checkout", contextId: "context" };
  const exact = {
    id: "preview-id", sourceConversationId: "source", sourceTitle: "Release checklist", destinationConversationId: destination.id,
    projectId: "owned-project", projectName: "Current repo", artifact: { kind: "summary", id: "exact-summary", sourceContextId: "saved-context", text: "Reviewed release plan." },
  };
  let attached: typeof exact[] = [];
  function result(id: string, title: string, projectId: string) {
    return { conversation: { id, title, checkoutId: id, contextId: "saved-context" },
      checkout: { projectId, checkoutId: id, repositoryRoot: "C:/repo", checkoutRoot: "C:/worktree" }, projectName: "Current repo" };
  }
  vi.mocked(invoke).mockImplementation(async (command, raw) => {
    const args = raw as Record<string, unknown>;
    switch (command) {
      case "checkout_conversation": return destination;
      case "conversation_draft": return saved;
      case "conversation_history": return { contextId: destination.contextId, messages: [] };
      case "conversation_reference_draft": return attached;
      case "conversation_attachment_draft": return { files: [], revision: "empty" };
      case "conversation_goal_overview": case "latest_conversation_job": case "latest_conversation_run": return null;
      case "conversation_models": case "pending_conversation_instructions": case "conversation_instruction_deliveries": return [];
      case "composer_suggestion_mode": return "off";
      case "conversation_library":
        expect(args).toEqual({ query: "Rel", projectId: exact.projectId });
        return [result("source", exact.sourceTitle, exact.projectId), result("foreign", "Release foreign", "foreign-project")];
      case "preview_conversation_reference":
        expect(args).toEqual({ destinationConversationId: destination.id, sourceConversationId: exact.sourceConversationId,
          selection: { kind: "automatic" } });
        return exact;
      case "edit_conversation_reference_draft": {
        expect(args.previewId).toBe(exact.id);
        if (args.attach) {
          const change = args.draftChange as { expected: string; replacement: string };
          expect(change.expected).toBe(saved);
          saved = change.replacement;
          attached = [exact];
        } else attached = [];
        return attached;
      }
      default: return null;
    }
  });
  const props = { params: { conversationId: destination.id, title: destination.title },
    api: { isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }) } } as unknown as ComponentProps<typeof ConversationPanel>;
  function mount() {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
    return render(<QueryClientProvider client={client}>
      <ConversationLibraryNavigation.Provider value={{ checkoutId: destination.checkoutId, projectId: exact.projectId, open: vi.fn() }}>
        <ConversationPanel {...props} />
      </ConversationLibraryNavigation.Provider>
    </QueryClientProvider>);
  }
  const user = userEvent.setup();
  const first = mount();
  const draft = await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" });
  await waitFor(() => expect(draft.value).toBe(saved));
  draft.focus();
  draft.setSelectionRange(9, 9);
  fireEvent.select(draft);
  const options = within(await screen.findByRole("listbox", { name: "Conversation mentions" }));
  expect(await options.findByRole("option", { name: /Release checklist/ })).toBeTruthy();
  expect(options.queryByText("Release foreign")).toBeNull();
  await user.keyboard("{Enter}");
  await waitFor(() => expect(draft.value).toBe("Read @Release checklist before approving."));
  expect(await screen.findByText("Release checklist · Summary")).toBeTruthy();
  first.unmount();
  mount();
  expect(await screen.findByText("Release checklist · Summary")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Remove reference to Release checklist" }));
  await waitFor(() => expect(screen.queryByText("Release checklist · Summary")).toBeNull());
});
