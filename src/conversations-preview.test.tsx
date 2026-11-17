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
  const library = screen.getByRole("region", { name: "Conversation library" });
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

test("starts and controls a goal separately from the transcript", async () => {
  const user = userEvent.setup();
  render(<Example />);
  const composer = screen.getByRole("textbox", { name: "Message draft" });
  await user.type(composer, "/goal");
  await user.click(screen.getByRole("button", { name: "Open Goal Mode" }));
  expect((screen.getByRole("combobox", { name: "Conversation mode" }) as HTMLSelectElement).value).toBe("goal");
  await user.type(composer, "Review the recovery checklist");
  await user.click(screen.getByRole("button", { name: "Start goal" }));
  const goal = screen.getByRole("region", { name: "Goal" });
  expect(within(goal).getByText("Review the recovery checklist")).toBeTruthy();
  await user.click(within(goal).getByRole("button", { name: "Pause goal" }));
  expect(within(goal).getByRole("status").textContent).toContain("Paused");
  await user.click(within(goal).getByRole("button", { name: "Resume goal" }));
  await user.click(within(goal).getByRole("button", { name: "Continue goal" }));
  expect(within(goal).getByRole("status").textContent).toContain("1/3 turns");
  await user.click(within(goal).getByRole("button", { name: "View goal objective" }));
  const dialog = screen.getByRole("dialog", { name: "Goal objective" });
  const objective = within(dialog).getByRole("textbox", { name: "Objective" }) as HTMLTextAreaElement;
  expect(objective.value).toBe("Review the recovery checklist");
  await user.clear(objective);
  await user.type(objective, "Review the backup and recovery checks");
  await user.click(within(dialog).getByRole("button", { name: "Save objective" }));
  expect(within(goal).getByText("Review the backup and recovery checks")).toBeTruthy();
  expect(within(goal).getByRole("status").textContent).toContain("Running · 1/3 turns");
  await user.click(within(goal).getByRole("button", { name: "View goal objective" }));
  expect(within(screen.getByRole("dialog", { name: "Goal objective" })).getByText("Review the recovery checklist")).toBeTruthy();
  await user.keyboard("{Escape}");
  await user.click(within(goal).getByRole("button", { name: "Cancel goal" }));
  expect(within(goal).getByRole("status").textContent).toContain("Cancelled");
  expect(screen.queryByText("/goal")).toBeNull();
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

test("highlights language-tagged code in agent messages and keeps other code readable", () => {
  const source = [
    "Inline `Write-Output`", "",
    "```ts", "const count = 42;", "```", "",
    "```powershell", 'Write-Output "hello"', "```", "",
    "```unregistered", "const count = 42;", "```", "",
    "```", "const count = 42;", "```",
  ].join("\n");
  const { container } = render(<ConversationMarkdown>{source}</ConversationMarkdown>);

  expect(container.querySelector("pre > code.language-ts.hljs .hljs-keyword")?.textContent).toBe("const");
  expect(container.querySelector("pre > code.language-powershell.hljs .hljs-built_in")?.textContent).toBe("Write-Output");
  expect(container.querySelector("pre > code.language-powershell")?.textContent).toBe('Write-Output "hello"\n');
  expect(container.querySelector("p > code")?.outerHTML).toBe("<code>Write-Output</code>");
  const plainBlocks = container.querySelectorAll("pre > code.language-unregistered, pre > code:not([class])");
  expect([...plainBlocks].map((code) => code.textContent)).toEqual(["const count = 42;\n", "const count = 42;\n"]);
  expect([...plainBlocks].every((code) => code.childElementCount === 0)).toBe(true);
});
