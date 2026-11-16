// @vitest-environment jsdom

import { useState, type ComponentProps } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { RunRecord } from "@/bindings/RunRecord";
import { ConversationPanel } from "@/conversation-panels";
import { ConversationSuggestionButton, useConversationSuggestion } from "@/conversation-suggestions";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.resetAllMocks(); });

const first: RunRecord = { id: "source-run", projectId: "project", checkoutId: "checkout", turnId: "turn", status: "running" };
const selected: ModelSelection = { provider: "codex", profileId: "current-profile", modelId: "working-model" };
const nativeCall = { conversationId: "chat", contextId: "completed-context", runId: first.id,
  selection: selected, effort: null, mode: "read_only" };
type Props = { run?: RunRecord; contextId?: string; selection?: ModelSelection; eligible?: boolean; liveFresh?: boolean; send: (text: string) => void };

function Composer({ run = first, contextId = "before-context", selection = selected, eligible = false, liveFresh, send }: Props) {
  const [state, setState] = useState({ draft: "", version: 0 });
  const edit = (draft: string) => setState((prior) => ({ draft, version: prior.version + 1 }));
  const suggestion = useConversationSuggestion({ conversationId: "chat", contextId, run, selection, effort: null,
    mode: "read_only", draft: state.draft, draftVersion: state.version, eligible, liveFresh, visible: true });
  const [answer, setAnswer] = useState("");
  return <>
    <form onSubmit={(event) => { event.preventDefault(); send(state.draft); }}>
      {suggestion && <ConversationSuggestionButton suggestion={suggestion} onAccept={() => edit(suggestion.text)} />}
      <label>Draft<textarea value={state.draft} onChange={(event) => edit(event.target.value)} onKeyDown={(event) => {
        if (suggestion && event.key === "Tab" && !event.shiftKey) { event.preventDefault(); edit(suggestion.text); }
      }} /></label>
      <button type="submit">Send</button>
    </form>
    <label>Agent question<input value={answer} onChange={(event) => setAnswer(event.target.value)} /></label>
  </>;
}

function fixture(mode: "native" | "omniagent" | "off", native: unknown, side: unknown = [], initial?: Props["run"]) {
  const send = vi.fn();
  const invokeMock = vi.mocked(invoke);
  invokeMock.mockImplementation(async (command) => {
    switch (command) {
      case "composer_suggestion_mode": return mode;
      case "composer_native_suggestion": return native;
      case "enqueue_conversation_side_request": return { status: "queued", jobId: "fallback-job" };
      case "conversation_side_requests": return side;
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const client = new QueryClient();
  client.setQueryData(["composer-suggestion-mode"], mode);
  const view = (props?: Omit<Props, "send">) => <QueryClientProvider client={client}><Composer send={send} {...props} /></QueryClientProvider>;
  const app = render(view({ run: initial }));
  return {
    user: userEvent.setup(), send, close: app.unmount,
    show: (props?: Omit<Props, "send">) => app.rerender(view(props)),
    finish: (props?: Omit<Props, "send">) => app.rerender(view({ run: { ...first, status: "succeeded" }, contextId: "completed-context", eligible: true, ...props })),
  };
}

it("prefers the native result, labels its source, and puts it in an editable draft until Send", async () => {
  const { user, send, finish } = fixture("omniagent", { kind: "available", text: "How should retries work?" });
  finish();
  expect(await screen.findByRole("button", { name: /Native CLI.*How should retries work/ })).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("composer_native_suggestion", nativeCall);
  expect(invoke).not.toHaveBeenCalledWith("enqueue_conversation_side_request", expect.anything());
  const draft = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Draft" });
  expect(draft.value).toBe("");
  await user.click(draft);
  await user.tab();
  expect(draft.value).toBe("How should retries work?");
  expect(send).not.toHaveBeenCalled();
  await user.type(draft, " Check both cases.");
  await user.click(screen.getByRole("button", { name: "Send" }));
  expect(send).toHaveBeenCalledWith("How should retries work? Check both cases.");
});

it("queues fallback only on explicit unavailability and consumes the exact isolated job", async () => {
  const answer = { purpose: "suggestion", contextId: "completed-context", sourceRunId: first.id,
    response: { outcome: { kind: "answered", text: "Where is the checkpoint saved?", route: "fresh" } } };
  const { user, send, finish } = fixture("omniagent", { kind: "unavailable" }, [
    { ...answer, job: { id: "unrelated-job" }, response: { outcome: { kind: "answered", text: "Unrelated response" } } },
    { ...answer, job: { id: "fallback-job" } },
  ]);
  finish();
  const button = await screen.findByRole("button", { name: /OmniAgent.*Where is the checkpoint saved/ });
  expect(screen.queryByText("Unrelated response")).toBeNull();
  expect(invoke).toHaveBeenCalledWith("enqueue_conversation_side_request", {
    conversationId: "chat", expectedContextId: "completed-context", expectedSourceRunId: first.id,
    selection: selected, effort: null, mode: "read_only", purpose: "suggestion", question: "",
  });
  await user.click(button);
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Draft" }).value).toBe("Where is the checkpoint saved?");
  expect(send).not.toHaveBeenCalled();
});

it("continues to accept typing and never restores a late response from the already queued job", async () => {
  let resolve!: (outcome: unknown) => void;
  const pending = new Promise((ready) => { resolve = ready; });
  const { user, finish, send } = fixture("omniagent", { kind: "unavailable" }, pending);
  finish();
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("conversation_side_requests", { conversationId: "chat" }));
  const draft = screen.getByRole("textbox", { name: "Draft" });
  await user.type(draft, "Write my own prompt");
  await user.clear(draft);
  await act(async () => resolve([{ job: { id: "fallback-job" }, purpose: "suggestion", contextId: "completed-context",
    sourceRunId: first.id, response: { outcome: { kind: "answered", text: "Late isolated suggestion" } } }]));
  expect(screen.queryByRole("button", { name: /Use suggested draft/ })).toBeNull();
  expect(send).not.toHaveBeenCalled();
});

it.each(["empty", "failed", "timed_out", "cancelled", "ineligible"])("leaves the draft empty and skips fallback after %s", async (kind) => {
  const { finish } = fixture("omniagent", { kind });
  finish();
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("composer_native_suggestion", nativeCall));
  expect(screen.queryByRole("button", { name: /Use suggested draft/ })).toBeNull();
  expect(invoke).not.toHaveBeenCalledWith("enqueue_conversation_side_request", expect.anything());
});

it("leaves fallback disabled in Native mode and skips lookup when Off or replaying a completed run", async () => {
  const native = fixture("native", { kind: "unavailable" });
  native.finish();
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("composer_native_suggestion", nativeCall));
  expect(invoke).not.toHaveBeenCalledWith("enqueue_conversation_side_request", expect.anything());
  native.close();
  vi.mocked(invoke).mockClear();

  const off = fixture("off", { kind: "available", text: "Unexpected" });
  off.finish();
  expect(invoke).not.toHaveBeenCalledWith("composer_native_suggestion", expect.anything());
  off.close();
  const replay = fixture("omniagent", { kind: "available", text: "Replayed suggestion" }, [], { ...first, status: "succeeded" });
  replay.finish();
  expect(invoke).not.toHaveBeenCalledWith("composer_native_suggestion", expect.anything());
  expect(screen.queryByText(/Replayed suggestion/)).toBeNull();
  replay.show({ run: first, liveFresh: false });
  replay.finish();
  expect(invoke).not.toHaveBeenCalledWith("composer_native_suggestion", expect.anything());
});

it.each(["edit", "model", "newer turn", "context", "question"] as const)("discards a late result after %s", async (change) => {
  let resolve!: (outcome: unknown) => void;
  const pending = new Promise((ready) => { resolve = ready; });
  const { user, finish } = fixture("omniagent", pending);
  finish();
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("composer_native_suggestion", nativeCall));
  if (change === "edit") {
    const draft = screen.getByRole("textbox", { name: "Draft" });
    await user.type(draft, "My own draft");
    await user.clear(draft);
  } else if (change === "model") {
    finish({ selection: { ...selected, modelId: "changed-model" } });
    finish();
  } else if (change === "newer turn") {
    finish({ run: { ...first, id: "new-run" }, eligible: false });
  } else if (change === "context") {
    finish({ contextId: "rewound-context", eligible: false });
  } else {
    finish({ eligible: false });
    await user.type(screen.getByRole("textbox", { name: "Agent question" }), "My answer");
  }
  await act(async () => resolve({ kind: "unavailable" }));
  expect(invoke).not.toHaveBeenCalledWith("enqueue_conversation_side_request", expect.anything());
  expect(screen.queryByRole("button", { name: /Use suggested draft/ })).toBeNull();
});

it("uses the real Conversation composer to accept a suggestion and sends only the edited draft", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  let run: RunRecord | null = null;
  let contextId = "before-context";
  let draft = "";
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "conversation_draft": return draft;
      case "save_conversation_draft": {
        if (args && "text" in args && typeof args.text === "string") draft = args.text;
        return;
      }
      case "conversation_history": return { contextId, messages: [] };
      case "conversation_events": return { cursor: "1", more: false, events: [] };
      case "latest_conversation_run": return run;
      case "latest_conversation_job": case "conversation_goal_overview": return null;
      case "conversation_models": return [{ efforts: [], defaultEffort: null, option: {
        selection: selected, name: "Fixture model", selectable: true, unavailableReason: null, rating: null, catalogVersion: 1,
      } }];
      case "composer_suggestion_mode": return "native";
      case "composer_native_suggestion": {
        expect(args).toEqual(nativeCall);
        return { kind: "available", text: "Explain retries" };
      }
      case "submit_conversation_continuation": return { status: "queued", job: { id: "operator-send" } };
      case "conversation_instruction_deliveries":
      case "pending_conversation_instructions":
      case "saved_conversation_questions":
      case "pending_conversation_inputs": return [];
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const api = { isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }) } as unknown as
    ComponentProps<typeof ConversationPanel>["api"];
  const containerApi = {} as ComponentProps<typeof ConversationPanel>["containerApi"];
  render(<QueryClientProvider client={client}><ConversationPanel api={api} containerApi={containerApi}
    params={{ conversationId: "chat", title: "Fixture conversation" }} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Model · Choose model" }));
  await user.click(within(screen.getByRole("menu", { name: "Model" })).getByRole("menuitemradio", { name: "Fixture model" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("composer_suggestion_mode"));
  run = first;
  await act(async () => client.invalidateQueries({ queryKey: ["conversation", "chat", "latest-run"] }));
  expect(await screen.findByRole("button", { name: "Stop" })).toBeTruthy();
  contextId = "completed-context";
  await act(async () => client.invalidateQueries({ queryKey: ["conversation", "chat", "messages"] }));
  run = { ...first, status: "succeeded" };
  await act(async () => client.invalidateQueries({ queryKey: ["conversation", "chat", "latest-run"] }));
  expect(await screen.findByRole("button", { name: /Native CLI.*Explain retries/ })).toBeTruthy();
  const input = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" });
  expect(input.value).toBe("");
  await user.click(input);
  await user.tab();
  expect(input.value).toBe("Explain retries");
  await waitFor(() => expect(draft).toBe("Explain retries"));
  expect(invoke).not.toHaveBeenCalledWith("submit_conversation_continuation", expect.anything());
  await user.type(input, " and timeouts");
  await user.click(screen.getByRole("button", { name: "Send" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("submit_conversation_continuation", {
    conversationId: "chat", expectedContextId: "completed-context", selection: selected,
    effort: null, mode: "read_only", text: "Explain retries and timeouts",
  }));
});
