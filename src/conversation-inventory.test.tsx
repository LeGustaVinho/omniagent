// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationInventory } from "@/conversation-inventory";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("loads saved inventory on demand and separates discovery from reported use", async () => {
  const guide = "C:/fixture/AGENTS.md";
  const localSkill = {
    id: "local", source: "filesystem", name: "shared", path: "C:/fixture/.agents/skills/shared/SKILL.md",
    scope: "checkout", pluginId: null, enabled: null, revision: "skill-local-r1",
  };
  const pluginSkill = {
    ...localSkill, id: "plugin", source: "provider", path: "C:/fixture/plugin/shared/SKILL.md", pluginId: "workflow",
  };
  vi.mocked(invoke).mockResolvedValue({
    dispatchId: "recorded-dispatch",
    inventory: {
      instructions: { sources: [{ path: guide, scope: "checkout", revision: "guide-r1", selection: "expected", importedFrom: null, includedBytes: null }], unresolved: [] },
      filesystemSkills: [localSkill],
      reportedSkills: { reported: true, entries: [pluginSkill] },
      requestedSkill: { name: "shared", matchingIds: ["local", "plugin"] },
      plugins: {
        id: "initial-inventory", checkedAt: "saved",
        report: { status: "reported", plugins: [{ id: "workflow", name: "Workflow", version: "1.2.0", enabled: false, scope: "project", installPath: "C:/fixture/plugin", sourceLocation: null }] },
      },
    },
    observations: [
      { eventSequence: "7", report: { kind: "file_read", path: "AGENTS.md", tool: "Read" }, targets: [{ kind: "instruction", index: 0 }] },
      { eventSequence: "8", report: { kind: "skill_use", name: "shared" }, targets: [] },
      { eventSequence: "9", report: { kind: "skill_use", name: "workflow:shared" }, targets: [{ kind: "skill", id: "plugin" }] },
    ],
  });
  const client = new QueryClient();
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}><ConversationInventory conversationId="chat-a" runId="run-a" /></QueryClientProvider>);

  expect(invoke).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "Dispatch inventory" }));

  expect(await screen.findByText("Recorded revision guide-r1")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("conversation_dispatch_inventory", { conversationId: "chat-a", runId: "run-a" });
  expect(screen.getByText("workflow · Version 1.2.0")).toBeTruthy();
  expect(screen.getByText("shared · Use unconfirmed")).toBeTruthy();
  expect(screen.getByText("shared · Use reported")).toBeTruthy();
  const reports = within(screen.getByRole("list", { name: "Provider inventory observations" }));
  const linkedGuide = reports.getByRole<HTMLAnchorElement>("link", { name: guide });
  expect(document.getElementById(linkedGuide.getAttribute("href")?.slice(1) ?? "")?.textContent).toContain("guide-r1");
  expect(reports.getByText("Saved identity unresolved")).toBeTruthy();
});
