// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, expect, it, vi } from "vitest";
import { WorkflowLaunchPicker, preferredWorkflowRevision } from "@/workflow-launch";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const ipc = vi.mocked(invoke);
const base = { id: "workflow", revision: "base-revision" };
const published = { id: "workflow", revision: "published-revision" };
const note = { id: "note-revision", relativePath: "docs/plan.md", sha256: "a".repeat(64) };

afterEach(() => {
  cleanup();
  ipc.mockReset();
});

it.each(["prompt", "markdown"])("sends only the selected request source (%s)", async (source) => {
  const submitted: unknown[] = [];
  ipc.mockImplementation(async (command, args) => {
    if (command === "workflow_authoring_bases") return [{
      version: published, name: "Request workflow",
      start: { fields: [], requiresDocument: false, requiresWork: false, requiresEvent: false, promptOrMarkdown: true },
    }];
    if (command === "registered_checkouts") return [
      { projectId: "project", checkoutId: "checkout", checkoutRoot: "Current checkout" },
    ];
    if (command === "project_work_hierarchy") return { work: [] };
    if (command === "list_repository_markdown_revisions") return [note];
    if (command === "read_repository_markdown") return { revision: note, markdown: "Pinned Markdown" };
    if (command === "launch_workflow_manual") {
      submitted.push(args);
      return { id: "request-run" };
    }
    throw new Error(command);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>
    <WorkflowLaunchPicker projectId="project" checkoutId="checkout" openWorkspace={vi.fn()} />
  </QueryClientProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Run published workflow" }));
  const prompt = await screen.findByRole("textbox", { name: "Prompt" });
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Start workflow" }).disabled).toBe(true);
  fireEvent.change(prompt, { target: { value: "Typed request" } });
  if (source === "markdown") {
    fireEvent.change(screen.getByRole("combobox", { name: "Request source" }), { target: { value: "markdown" } });
    await screen.findByText("Pinned Markdown");
  }
  const button = screen.getByRole<HTMLButtonElement>("button", { name: "Start workflow" });
  await waitFor(() => expect(button.disabled).toBe(false));
  fireEvent.click(button);
  await waitFor(() => expect(submitted).toEqual([{
    request: {
      projectId: "project", workflow: published, checkoutId: "checkout", workId: null,
      requestId: expect.any(String), documentRevisionId: source === "markdown" ? note.id : null,
      inputs: source === "prompt" ? { prompt: "Typed request" } : {},
    },
  }]));
});

it.each([undefined, "selected-subtask"])("launches the selected immutable version for work %s", async (workId) => {
  const submitted: unknown[] = [];
  ipc.mockImplementation(async (command, args) => {
    if (command === "workflow_authoring_bases") return [
      { version: base, name: "Older pipeline" }, { version: published, name: "Published pipeline" },
    ];
    if (command === "list_repository_markdown_revisions") return [note];
    if (command === "read_repository_markdown") return { revision: note, markdown: "Pinned Markdown" };
    if (command === "launch_workflow_manual") {
      submitted.push(args);
      if (submitted.length === 1) throw new Error("Response interrupted");
      return { id: "run" };
    }
    throw new Error(command);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const target = vi.fn();
  const preferred = preferredWorkflowRevision({ base, publication: { workflow: published } });
  render(<QueryClientProvider client={client}>
    <WorkflowLaunchPicker projectId="project" checkoutId="checkout" openWorkspace={target}
      preferred={preferred} workId={workId} />
  </QueryClientProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Run published workflow" }));
  expect((await screen.findByRole("combobox", { name: "Published workflow revision" }) as HTMLSelectElement)
    .selectedOptions[0]?.textContent).toContain("Published pipeline");
  expect(await screen.findByText("Pinned Markdown")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start workflow" }));
  expect((await screen.findByRole("alert")).textContent).toContain("Response interrupted");
  fireEvent.click(screen.getByRole("button", { name: "Start workflow" }));
  await waitFor(() => expect(target).toHaveBeenCalledWith("pipelines", "run:run"));
  expect(submitted).toHaveLength(2);
  expect(submitted[0]).toMatchObject({
    request: { projectId: "project", workflow: published, workId: workId ?? null,
      source: { checkoutId: "checkout", revisionId: note.id }, requestId: expect.any(String) },
  });
  expect(submitted[1]).toEqual(submitted[0]);
});

it("launches typed inputs in the selected project without a Markdown revision and reuses the retry identity", async () => {
  const submitted: unknown[] = [];
  ipc.mockImplementation(async (command, args) => {
    if (command === "workflow_authoring_bases") return [{ version: published, name: "Typed pipeline", start: {
      fields: [
        { name: "title", valueType: "text", required: true },
        { name: "enabled", valueType: "boolean", required: true },
        { name: "empty", valueType: "null", required: false },
      ],
      requiresDocument: false, requiresWork: true, requiresEvent: false,
    } }];
    if (command === "registered_checkouts") return [
      { projectId: "project", checkoutId: "checkout", checkoutRoot: "Current checkout" },
      { projectId: "project", checkoutId: "other-checkout", checkoutRoot: "Second checkout" },
      { projectId: "foreign", checkoutId: "foreign", checkoutRoot: "Foreign checkout" },
    ];
    if (command === "project_work_hierarchy") return { work: [
      { task: { id: "scoped-work", title: "Scoped work" }, placement: { kind: "task", epicId: null } },
    ] };
    if (command === "launch_workflow_manual") {
      submitted.push(args);
      if (submitted.length === 1) throw new Error("Response interrupted");
      return { id: "typed-run" };
    }
    throw new Error(command);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const target = vi.fn();
  render(<QueryClientProvider client={client}>
    <WorkflowLaunchPicker projectId="project" checkoutId="checkout" openWorkspace={target} />
  </QueryClientProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Run published workflow" }));
  await screen.findByRole("option", { name: "Second checkout" });
  expect(screen.queryByRole("option", { name: "Foreign checkout" })).toBeNull();
  const start = screen.getByRole<HTMLButtonElement>("button", { name: "Start workflow" });
  expect(start.disabled).toBe(true);
  fireEvent.change(screen.getByRole("combobox", { name: "Checkout" }), { target: { value: "other-checkout" } });
  await screen.findByRole("option", { name: "Task · Scoped work" });
  fireEvent.change(screen.getByRole("combobox", { name: "Task or Sub-Task" }), { target: { value: "scoped-work" } });
  fireEvent.change(screen.getByRole("textbox", { name: "title" }), { target: { value: "Pinned text" } });
  fireEvent.change(screen.getByRole("combobox", { name: "enabled" }), { target: { value: "false" } });
  fireEvent.click(screen.getByRole("checkbox", { name: "Provide empty" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "Dispatch ready workflow stages automatically" }));
  expect(start.disabled).toBe(false);
  fireEvent.click(start);
  expect((await screen.findByRole("alert")).textContent).toContain("Response interrupted");
  fireEvent.click(start);
  await waitFor(() => expect(target).toHaveBeenCalledWith("pipelines", "run:typed-run"));
  expect(submitted).toEqual([{
    request: { projectId: "project", workflow: published, checkoutId: "other-checkout", documentRevisionId: null,
      workId: "scoped-work", inputs: { title: "Pinned text", enabled: false, empty: null },
      automatic: true, requestId: expect.any(String) },
  }, submitted[0]]);
  expect(ipc.mock.calls.some(([command]) => command === "list_repository_markdown_revisions")).toBe(false);
});
