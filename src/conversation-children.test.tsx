// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationAgentTree, ConversationChildren, recordChildActivity, type AgentTree, type ChildActivity, type ChildLocation, type Delegation } from "@/conversation-children";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("scopes stable children by provider session and parent and retains unidentified parent activity", async () => {
  const delegation: Delegation = { children: {}, parentActivity: {} };
  const location: ChildLocation = { sessionId: "session-a", parentItemId: "parent-a", turnId: "turn", itemId: "call" };
  const started: ChildActivity = {
    type: "child_activity", child_id: "child", status: "started",
    details: { parentId: null, prompt: "Inspect the fixture", requestedModel: "requested-model", reportedModel: null },
  };
  recordChildActivity(delegation, started, location);
  recordChildActivity(delegation, { ...started, details: undefined }, { ...location, parentItemId: "parent-b" });
  recordChildActivity(delegation, { ...started, details: undefined }, { ...location, sessionId: "session-b" });
  recordChildActivity(delegation, {
    ...started, status: "completed",
    details: { ...started.details!, response: "Fixture child answer" },
  }, location);
  recordChildActivity(delegation, { type: "child_activity", child_id: null, status: "delegating" }, location);
  render(<ConversationChildren delegation={delegation} />);
  expect(screen.getAllByText(/^Native child child · /)).toHaveLength(3);
  expect(screen.getByText("Parent delegation · delegating")).toBeDefined();
  const completed = screen.getByText("Native child child · completed");
  await userEvent.click(completed);
  const details = within(completed.closest("details")!);
  expect(details.getByText("session-a")).toBeDefined();
  expect(details.getByText("parent-a")).toBeDefined();
  expect(details.getByText("Inspect the fixture")).toBeDefined();
  expect(details.getByText("Requested model")).toBeDefined();
  expect(details.getByText("requested-model")).toBeDefined();
  expect(details.getByText("Fixture child answer")).toBeDefined();
  expect(details.getByText("Context provenance")).toBeDefined();
  expect(details.getByText("Reasoning")).toBeDefined();
  expect(details.getByText("Controls")).toBeDefined();
  expect(details.getAllByText("Unreported")).toHaveLength(4);
});

it("links unique native parents in the same session and retains unidentified activity", async () => {
  const delegation: Delegation = { children: {}, parentActivity: {} };
  const location: ChildLocation = { sessionId: "native-session", parentItemId: null, turnId: "turn", itemId: "tool" };
  recordChildActivity(delegation, { type: "child_activity", child_id: "parent", status: "running" }, location);
  recordChildActivity(delegation, { type: "child_activity", child_id: "child", status: "running", details: {
    parentId: "parent", prompt: null, requestedModel: null, reportedModel: null,
  } }, location);
  recordChildActivity(delegation, { type: "child_activity", child_id: "tool-child", status: "running" },
    { ...location, parentItemId: "parent" });
  recordChildActivity(delegation, { type: "child_activity", child_id: "missing", status: "notFound" }, location);
  render(<ConversationChildren delegation={delegation} />);
  const parent = screen.getByText("Native child parent · running").closest("li");
  if (!parent) throw new Error("Missing parent node");
  const descendants = within(parent).getByRole("list", { name: "Native descendants" });
  await userEvent.click(within(descendants).getByText("Native child child · running"));
  expect(within(descendants).getByRole("link", { name: "parent" }).getAttribute("href")).toBe(`#${parent.id}`);
  expect(within(descendants).queryByText("Native child tool-child · running")).toBeNull();
  expect(screen.getByText("Native child tool-child · running")).toBeTruthy();
  expect(screen.queryByText(/^Native child missing/)).toBeNull();
  expect(screen.getByText("Parent delegation · notFound")).toBeTruthy();
});

it("keeps managed attempts nested under their actual parent and opens retry history and the recorded attempt", async () => {
  const root = { runId: "root", conversationId: "root-chat" };
  const base = { ...root, ordinal: 1, previousRunId: null, status: "failed", provider: "codex",
    model: "requested", reportedModel: null, promptDelivery: null, contextRecorded: false } as const;
  const original = { ...base, runId: "previous-root" };
  const selected = { ...base, ordinal: 2, status: "running", previousRunId: original.runId } as const;
  const child = { ...base, runId: "child", conversationId: "child-chat", provider: "claude", status: "interrupted" } as const;
  const retry = { ...child, runId: "child-retry", ordinal: 2, status: "running", previousRunId: child.runId,
    reportedModel: "observed-model", promptDelivery: "submitted", contextRecorded: true } as const;
  const fixture: AgentTree = {
    parent: null, attempts: [original, selected],
    managed: [
      { id: "child", parent: root, queueState: "queued", queuedRecovery: false, attempts: [child, retry] },
      { id: "grandchild", parent: retry, queueState: "queued", queuedRecovery: false, attempts: [] },
    ],
    nativeObservations: [{ runId: retry.runId, event: {
      location: { sessionId: null, parentItemId: null, turnId: "certified", itemId: "call" },
      data: { type: "child_activity", child_id: "native-worker", status: "running" },
    } }],
  };
  vi.mocked(invoke).mockResolvedValue(fixture);
  const openActivity = vi.fn();
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <ConversationAgentTree {...root} live openActivity={openActivity} />
  </QueryClientProvider>);
  const tree = await screen.findByRole("region", { name: "Agent tree" });
  expect(invoke).toHaveBeenCalledWith("conversation_agent_tree", { conversationId: "root-chat", runId: "root" });
  await userEvent.click(within(tree).getByText("Attempt history"));
  const history = within(tree).getByText("Attempt history").closest("details");
  if (!history) throw new Error("Missing attempt history");
  await userEvent.click(within(history).getByRole("button", { name: "Open previous attempt" }));
  expect(openActivity).toHaveBeenLastCalledWith("previous-root", "root-chat");
  const managed = within(tree).getByText("Managed child · Claude Code · Running").closest("li");
  if (!managed) throw new Error("Missing managed child");
  expect(within(managed).getByText("observed-model")).toBeTruthy();
  expect(within(managed).getByText("Native child native-worker · running")).toBeTruthy();
  await userEvent.click(within(managed).getByRole("button", { name: "Open attempt 2" }));
  expect(openActivity).toHaveBeenLastCalledWith("child-retry", "child-chat");
  const grandchild = within(managed).getByText("Managed child · Queued").closest("li");
  if (!grandchild) throw new Error("Missing second-level managed child");
  await userEvent.click(within(grandchild).getByRole("button", { name: "Open parent attempt" }));
  expect(openActivity).toHaveBeenLastCalledWith("child-retry", "child-chat");
});
