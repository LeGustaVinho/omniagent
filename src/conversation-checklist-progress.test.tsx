// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationChecklistProgress } from "@/conversation-checklist-progress";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it.each(["track_progress", "run_by_item"] as const)("supports manual check and reopen in %s without agent access", async (mode) => {
  const selected = { id: "item-1", revision: 3, body: "Check HTTP headers", checked: false };
  const other = { id: "item-2", revision: 1, body: "Check retry policy", checked: false };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "conversation_checklist_progress") return {
      access: { taskId: "task-id", mode, availability: "bridge_unavailable", selectedItemId: mode === "run_by_item" ? selected.id : null },
      checklist: { id: "checklist-id", items: [{ ...selected }, other] },
    };
    if (command === "set_conversation_checklist_item_checked" && args && "checked" in args) {
      expect(args).toMatchObject({ taskId: "task-id", itemId: selected.id, expectedRevision: selected.revision });
      selected.checked = Boolean(args.checked);
      selected.revision += 1;
      return { itemId: selected.id, revision: selected.revision, checked: selected.checked };
    }
    throw new Error(`Unexpected command ${command}`);
  });
  const user = userEvent.setup();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}>
    <ConversationChecklistProgress projectId="project-id" runId="run-id" live={false} />
  </QueryClientProvider>);
  await user.click(await screen.findByText("Checklist progress", { selector: "summary" }));
  expect(screen.getByText(/Agent updates are unavailable/)).toBeTruthy();
  expect(screen.queryAllByRole("checkbox")).toHaveLength(mode === "track_progress" ? 2 : 1);
  const item = screen.getByRole<HTMLInputElement>("checkbox", { name: selected.body });
  await user.click(item);
  await waitFor(() => expect(item.checked).toBe(true));
  await user.click(item);
  await waitFor(() => expect(item.checked).toBe(false));
  expect(invoke).toHaveBeenLastCalledWith("conversation_checklist_progress", { projectId: "project-id", runId: "run-id" });
  expect(invoke).toHaveBeenCalledWith("set_conversation_checklist_item_checked", {
    projectId: "project-id", taskId: "task-id", itemId: selected.id, expectedRevision: 4, checked: false,
  });
});
