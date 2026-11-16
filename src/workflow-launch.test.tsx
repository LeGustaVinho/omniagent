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
  await waitFor(() => expect(target).toHaveBeenCalledWith("pipelines", "run"));
  expect(submitted).toHaveLength(2);
  expect(submitted[0]).toMatchObject({
    request: { projectId: "project", workflow: published, workId: workId ?? null,
      source: { checkoutId: "checkout", revisionId: note.id }, requestId: expect.any(String) },
  });
  expect(submitted[1]).toEqual(submitted[0]);
});
