// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, expect, it, vi } from "vitest";
import { WorkflowTriggers } from "@/workflow-triggers";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const ipc = vi.mocked(invoke);
afterEach(() => { cleanup(); ipc.mockReset(); });

it("activates a project rule for a selected immutable workflow and shows its execution link", async () => {
  const pipeline = { id: "workflow", revision: "workflow-r1" };
  const note = { id: "note-r1", relativePath: "brief.md", sha256: "a".repeat(64) };
  ipc.mockImplementation(async (command, args) => {
    switch (command) {
      case "workflow_trigger_rules": return [];
      case "workflow_trigger_activations": return [{
        ruleId: "a", ruleName: "Review", ruleRevision: 1, eventId: "event-a",
        workId: "work-a", runId: "run-a", outcome: "launched", recordedAt: "2026-11-16T00:00:00Z",
      }];
      case "workflow_authoring_bases": return [{ version: pipeline, name: "Review workflow" }];
      case "list_repository_markdown_revisions": return [note];
      case "read_repository_markdown": return { revision: note, markdown: "Pinned note" };
      case "save_workflow_trigger_rule": return args;
      default: throw new Error(command);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const openWorkspace = vi.fn();
  render(<QueryClientProvider client={client}>
    <WorkflowTriggers checkout={{ projectId: "current", checkoutId: "worktree", repositoryRoot: ".", checkoutRoot: "." }}
      openWorkspace={openWorkspace} />
  </QueryClientProvider>);
  fireEvent.click(await screen.findByRole("button", { name: "Open execution" }));
  expect(openWorkspace).toHaveBeenCalledWith("pipelines", "run:run-a");
  fireEvent.click(screen.getByRole("button", { name: "New rule" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Rule name" }), { target: { value: "Review on Ready" } });
  fireEvent.change(screen.getByRole("combobox", { name: "Event" }), { target: { value: "state_changed" } });
  fireEvent.change(screen.getByRole("combobox", { name: "New status" }), { target: { value: "ready" } });
  fireEvent.change(screen.getByRole("combobox", { name: "Work item type" }), { target: { value: "subtask" } });
  await screen.findByRole("option", { name: /Review workflow/ });
  fireEvent.change(screen.getByRole("combobox", { name: "Pipeline revision" }), { target: { value: JSON.stringify([pipeline.id, pipeline.revision]) } });
  expect(await screen.findByText("Pinned note")).toBeTruthy();
  const save = screen.getByRole("button", { name: "Save rule" }) as HTMLButtonElement;
  expect(save.disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Use this Markdown revision" }));
  const enable = screen.getByRole("checkbox", { name: "Enable rule" }) as HTMLInputElement;
  expect(enable.checked).toBe(false);
  fireEvent.click(enable);
  fireEvent.click(save);
  await waitFor(() => expect(ipc).toHaveBeenCalledWith("save_workflow_trigger_rule", {
    projectId: "current", ruleId: null, expectedRevision: null,
    spec: { name: "Review on Ready", enabled: true, event: "state_changed", targetState: "ready",
      work: "subtask", pipeline, source: { checkoutId: "worktree", revisionId: note.id } },
  }));
});

it("allows a scoped checkout without a note for event workflows and labels manual-only revisions", async () => {
  const pipeline = { id: "workflow", revision: "event-revision" };
  const manual = { id: "workflow", revision: "manual-revision" };
  const start = { fields: [], requiresDocument: false, requiresWork: false, requiresEvent: true };
  ipc.mockImplementation(async (command, args) => {
    switch (command) {
      case "workflow_trigger_rules": return [];
      case "workflow_trigger_activations": return [];
      case "workflow_authoring_bases": return [
        { version: pipeline, name: "Event pipeline", start },
        { version: manual, name: "Manual pipeline", start: { ...start, requiresEvent: false,
          fields: [{ name: "title", valueType: "text", required: true }],
        } },
      ];
      case "registered_checkouts": return [
        { projectId: "current", checkoutId: "worktree", checkoutRoot: "Current checkout" },
        { projectId: "current", checkoutId: "other", checkoutRoot: "Other checkout" },
        { projectId: "foreign", checkoutId: "foreign", checkoutRoot: "Foreign checkout" },
      ];
      case "save_workflow_trigger_rule": return args;
      default: throw new Error(command);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>
    <WorkflowTriggers checkout={{ projectId: "current", checkoutId: "worktree", repositoryRoot: ".", checkoutRoot: "." }}
      openWorkspace={vi.fn()} />
  </QueryClientProvider>);
  fireEvent.click(screen.getByRole("button", { name: "New rule" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Rule name" }), { target: { value: "Run when created" } });
  const unavailable = await screen.findByRole<HTMLOptionElement>("option", { name: /Manual pipeline.*Manual launch only/ });
  expect(unavailable.disabled).toBe(true);
  fireEvent.change(screen.getByRole("combobox", { name: "Pipeline revision" }), {
    target: { value: JSON.stringify([pipeline.id, pipeline.revision]) },
  });
  await screen.findByRole("option", { name: "Other checkout" });
  expect(screen.queryByRole("option", { name: "Foreign checkout" })).toBeNull();
  expect(screen.queryByRole("region", { name: "Repository Markdown source" })).toBeNull();
  fireEvent.change(screen.getByRole("combobox", { name: "Checkout" }), { target: { value: "other" } });
  fireEvent.click(screen.getByRole("checkbox", { name: "Enable rule" }));
  fireEvent.click(screen.getByRole("button", { name: "Save rule" }));
  await waitFor(() => expect(ipc).toHaveBeenCalledWith("save_workflow_trigger_rule", {
    projectId: "current", ruleId: null, expectedRevision: null,
    spec: { name: "Run when created", enabled: true, event: "created", targetState: null,
      work: "all", pipeline, source: { checkoutId: "other" } },
  }));
});
