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
  expect(openWorkspace).toHaveBeenCalledWith("pipelines", "run-a");
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
