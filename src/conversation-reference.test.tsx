// @vitest-environment jsdom

import { useState } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationReferencePicker } from "@/conversation-library";
import { ConversationReferenceChips, type ConversationReferencePreview } from "@/conversation-reference";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const projectId = "project-approved";
const destinationConversationId = "destination";
const sourceConversationId = "source";
const sourceTitle = "Saved source";
const projectName = "Release";
const contextId = "original-context";
const source = { id: sourceConversationId, checkoutId: "source-worktree", title: sourceTitle, contextId };
const scope = { destinationConversationId, sourceConversationId };
const checkout = { projectId, checkoutId: source.checkoutId, checkoutRoot: "C:/release/source", repositoryRoot: "C:/release" };

const metadata = { checkout, projectName, agent: "codex", model: "model", status: "succeeded", lastActivityAt: null };
const summaries = [{ artifactId: "selected-summary", contextId: "summary-source-context", text: "Frozen summary text" }];
const messages = [
  { messageId: "first", revisionId: "first-revision", role: "user", text: "First source message\n\nExact spacing." },
  { messageId: "middle", revisionId: "middle-revision", role: "assistant", text: "Unselected middle message" },
  { messageId: "last", revisionId: "last-revision", role: "assistant", text: "Last source message" },
] as const;

const baseReference = { ...scope, id: "preview-token", sourceTitle, projectId, projectName };
const excerpt: ConversationReferencePreview = {
  ...baseReference,
  artifact: {
    kind: "excerpt", id: "saved-excerpt-revision", sourceContextId: contextId,
    excerpts: [messages[0], messages[2]].map(({ role, text, revisionId }) => ({
      role, text, source: { origin: { revisionId }, byteRange: { start: 0, end: text.length } },
    })),
  },
};
const summary: ConversationReferencePreview = {
  ...baseReference,
  artifact: { kind: "summary", id: summaries[0]!.artifactId, sourceContextId: summaries[0]!.contextId, text: summaries[0]!.text },
};

function mount() {
  const attached = vi.fn();
  const opened = vi.fn(async () => {});
  function Composer() {
    const [references, setReferences] = useState<ConversationReferencePreview[]>([]);
    return <>
      <ConversationReferencePicker projectId={projectId} excludeId={destinationConversationId} onOpen={opened}
        onAttach={(reference) => { attached(reference); setReferences((current) => [...current, reference]); }} />
      <ConversationReferenceChips references={references} onRemove={(id) =>
        setReferences((current) => current.filter((reference) => reference.id !== id))} />
    </>;
  }
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><Composer /></QueryClientProvider>);
  return { attached, opened, user: userEvent.setup() };
}

function referenceServer(expectedSelection: object, reference: ConversationReferencePreview) {
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "conversation_library":
        expect(args).toEqual({ query: null, projectId });
        return [
          { ...metadata, conversation: source },
          { ...metadata, conversation: { ...source, id: destinationConversationId, title: "Current conversation" } },
          { ...metadata, checkout: { ...checkout, projectId: "outside" }, conversation: { ...source, id: "outside", title: "Other project" } },
        ];
      case "conversation_reference_source":
        expect(args).toEqual(scope);
        return { source, contextId, projectId, projectName, messages, summaries, retainedContextIds: [contextId] };
      case "preview_conversation_reference":
        expect(args).toEqual({ ...scope, selection: expectedSelection });
        return reference;
      default: throw new Error(`Unexpected invoke ${command}`);
    }
  });
}

async function chooseReference(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Find conversation references" }));
  const picker = within(await screen.findByRole("dialog", { name: "Conversation references" }));
  const choose = await picker.findByRole("button", { name: `Choose excerpt or summary from ${sourceTitle}` });
  expect(picker.getByRole("button", { name: `Open source ${sourceTitle}` })).toBeTruthy();
  expect(picker.queryByRole("button", { name: "Open source Current conversation" })).toBeNull();
  expect(picker.queryByRole("button", { name: "Open source Other project" })).toBeNull();
  await user.click(choose);
  return picker;
}

it("pins selected messages in transcript order, previews the exact excerpt and removes its draft chip", async () => {
  referenceServer({
    kind: "messages", contextId, spans: [{ revisionId: messages[0].revisionId }, { revisionId: messages[2].revisionId }],
  }, excerpt);
  const { user, attached, opened } = mount();
  const picker = await chooseReference(user);
  await user.click(await picker.findByRole("checkbox", { name: /Last source message/ }));
  await user.click(picker.getByRole("checkbox", { name: /First source message/ }));
  await user.click(picker.getByRole("button", { name: "Preview selection" }));
  const exact = await picker.findByRole("region", { name: "Exact conversation reference" });
  expect(within(exact).getByText(projectName)).toBeTruthy();
  expect(within(exact).getByText(contextId)).toBeTruthy();
  expect([...exact.querySelectorAll("pre")].map((item) => item.textContent)).toEqual([messages[0].text, messages[2].text]);
  expect(attached).not.toHaveBeenCalled();
  await user.click(picker.getByRole("button", { name: "Attach reference" }));
  expect(attached).toHaveBeenCalledExactlyOnceWith(excerpt);
  expect(opened).not.toHaveBeenCalled();
  const chips = within(screen.getByRole("list", { name: "Attached conversation references" }));
  await user.click(chips.getByText(`${sourceTitle} · Excerpt`));
  expect(chips.getByText("saved-excerpt-revision")).toBeTruthy();
  await user.click(chips.getByRole("button", { name: `Remove reference to ${sourceTitle}` }));
  expect(screen.queryByRole("list", { name: "Attached conversation references" })).toBeNull();
});

it("attaches the selected saved summary with its original source revision", async () => {
  referenceServer({ kind: "summary", artifactId: summaries[0]!.artifactId }, summary);
  const { user, attached } = mount();
  const picker = await chooseReference(user);
  await user.click(await picker.findByRole("button", { name: "Saved summary" }));
  await user.selectOptions(picker.getByRole("combobox", { name: "Summary revision" }), summaries[0]!.artifactId);
  await user.click(picker.getByRole("button", { name: "Preview selection" }));
  const exact = within(await picker.findByRole("region", { name: "Exact conversation reference" }));
  expect(exact.getByText(summaries[0]!.contextId)).toBeTruthy();
  expect(exact.getByText(summaries[0]!.text)).toBeTruthy();
  await user.click(picker.getByRole("button", { name: "Attach reference" }));
  expect(attached).toHaveBeenCalledExactlyOnceWith(summary);
});
