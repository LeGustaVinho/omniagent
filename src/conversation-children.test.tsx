// @vitest-environment jsdom
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it } from "vitest";
import { ConversationChildren, recordChildActivity, type ChildActivity, type ChildLocation, type Delegation } from "@/conversation-children";

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
  expect(screen.getAllByText(/^Child child · /)).toHaveLength(3);
  expect(screen.getByText("Parent delegation · delegating")).toBeDefined();
  const completed = screen.getByText("Child child · completed");
  await userEvent.click(completed);
  const details = within(completed.closest("details")!);
  expect(details.getByText("session-a")).toBeDefined();
  expect(details.getByText("parent-a")).toBeDefined();
  expect(details.getByText("Inspect the fixture")).toBeDefined();
  expect(details.getByText("Requested model")).toBeDefined();
  expect(details.getByText("requested-model")).toBeDefined();
  expect(details.getByText("Fixture child answer")).toBeDefined();
  expect(details.getByText("Context provenance")).toBeDefined();
  expect(details.getAllByText("Unavailable")).toHaveLength(2);
});
