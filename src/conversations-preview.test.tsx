// @vitest-environment jsdom
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { ConversationMarkdown } from "@/conversation-markdown";
import ConversationsPreview from "@/conversations-preview";
import { exampleCheckout, nextDeliveryStep, type DeliveryStep } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

function Example({ initialStep = "note", openWorkspace = vi.fn() }: {
  initialStep?: DeliveryStep;
  openWorkspace?: React.ComponentProps<typeof ConversationsPreview>["openWorkspace"];
}) {
  const [delivery, setDelivery] = useState(initialStep);
  return <ConversationsPreview checkout={exampleCheckout} simulation={{
    scenario: "delivery", epoch: 1, delivery, advanceDelivery: (action) => setDelivery((step) => nextDeliveryStep(step, action)),
  }} target="plan" startSimulation={vi.fn()} openWorkspace={openWorkspace} openActivity={vi.fn()} openChanges={vi.fn()} />;
}

test("answers and approves the seeded plan before opening the linked task without desktop IPC", async () => {
  const user = userEvent.setup();
  const openWorkspace = vi.fn();
  render(<Example initialStep="question" openWorkspace={openWorkspace} />);
  await user.click(screen.getByRole("radio", { name: "Developers" }));
  await user.click(screen.getByRole("button", { name: "Reply" }));
  expect(screen.getByText("Use developers as the audience.")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Approve implementation" }));
  expect(screen.getByText("Queued")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Open release task" }));
  expect(openWorkspace).toHaveBeenCalledExactlyOnceWith("tasks", "release");
  expect(invoke).not.toHaveBeenCalled();
});

test("context actions target the clicked conversation while another conversation remains open", async () => {
  const user = userEvent.setup();
  render(<Example />);
  const library = screen.getByRole("complementary", { name: "Conversation library" });
  fireEvent.contextMenu(within(library).getByRole("button", { name: "Check provider profiles" }));
  await user.click(screen.getByRole("menuitem", { name: "Rename" }));
  const dialog = screen.getByRole("dialog", { name: "Rename conversation" });
  const title = within(dialog).getByRole("textbox", { name: "Conversation title" });
  await user.clear(title);
  await user.type(title, "Inspect qualified profiles");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(library).getByRole("button", { name: "Inspect qualified profiles" })).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Plan the release checklist" })).toBeTruthy();
  expect(invoke).not.toHaveBeenCalled();
});

test("renders Markdown links and images as text inside agent messages", () => {
  const { container } = render(<ConversationMarkdown>{
    "[Docs](https://example.com/path)\n\n![Diagram](https://example.com/image.png)\n\n"
      + "<iframe src='https://example.com/embed'></iframe>\n\n**Check**"
  }</ConversationMarkdown>);
  expect(container.textContent).toContain("https://example.com/path");
  expect(container.textContent).toContain("Diagram");
  expect(container.querySelector("a,img,iframe")).toBeNull();
  expect(container.querySelector("strong")?.textContent).toEqual("Check");
});
