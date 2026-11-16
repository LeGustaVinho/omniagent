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
  const discoveredOnly = {
    ...localSkill, id: "discovery-only", name: "discovered-only", path: "C:/fixture/.agents/skills/discovered-only/SKILL.md",
  };
  const saved = {
    dispatchId: "recorded-dispatch",
    checkout: {
      runId: "run-a", checkoutId: "managed-checkout",
      workingDirectory: "C:/worktrees/implementation/src", worktreeRoot: "C:/worktrees/implementation",
      primaryRoot: "C:/fixture",
    },
    inheritedCheckout: {
      runId: "source-run", checkoutId: "source-checkout",
      workingDirectory: "C:/old-worktrees/review", worktreeRoot: null, primaryRoot: null,
    },
    nativeHistory: { mode: "provider_cursor", sessionId: "native-session", contextId: "source-context" },
    historyReports: [{ eventSequence: "4", turns: 1, itemKind: null }],
    inventory: {
      instructions: { sources: [{ path: guide, scope: "checkout", revision: "guide-r1", selection: "expected", importedFrom: null, includedBytes: null }], unresolved: [] },
      filesystemSkills: [localSkill, discoveredOnly],
      reportedSkills: { reported: true, entries: [pluginSkill] },
      requestedSkill: { name: "shared", matchingIds: ["local", "plugin"] },
      plugins: {
        id: "initial-inventory", checkedAt: "saved",
        report: { status: "reported", plugins: [{ id: "workflow", name: "Workflow", version: "1.2.0", enabled: true, scope: "project", installPath: "C:/fixture/plugin", sourceLocation: null }] },
      },
    },
    observations: [
      { eventSequence: "7", report: { kind: "file_read", path: "AGENTS.md", tool: "Read" }, targets: [{ kind: "instruction", index: 0 }] },
      { eventSequence: "8", report: { kind: "skill_use", name: "shared" }, targets: [] },
      { eventSequence: "9", report: { kind: "skill_use", name: "workflow:shared" }, targets: [{ kind: "skill", id: "plugin" }] },
    ],
  };
  const fresh = {
    ...saved,
    dispatchId: "new-dispatch",
    inventory: {
      ...saved.inventory,
      instructions: { ...saved.inventory.instructions, sources: saved.inventory.instructions.sources.map((source) => ({ ...source, revision: "guide-r2" })) },
      filesystemSkills: [localSkill],
      plugins: {
        ...saved.inventory.plugins,
        report: {
          ...saved.inventory.plugins.report,
          plugins: saved.inventory.plugins.report.plugins.map((plugin) => ({ ...plugin, version: "2.0.0", enabled: false })),
        },
      },
    },
    observations: [],
  };
  vi.mocked(invoke).mockImplementation(async (_command, args) => {
    return args && "runId" in args && args.runId === "run-b" ? fresh : saved;
  });
  const client = new QueryClient();
  const user = userEvent.setup();
  const attempt = (runId: string) => <QueryClientProvider client={client}>
    <ConversationInventory key={runId} conversationId="chat-a" runId={runId} />
  </QueryClientProvider>;
  const view = render(attempt("run-a"));

  expect(invoke).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "Dispatch inventory" }));

  expect(await screen.findByText("Recorded revision guide-r1")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("conversation_dispatch_inventory", { conversationId: "chat-a", runId: "run-a" });
  expect(screen.getByText("workflow · Version 1.2.0")).toBeTruthy();
  expect(screen.getByText("C:/worktrees/implementation/src")).toBeTruthy();
  expect(screen.getByText("C:/worktrees/implementation")).toBeTruthy();
  expect(screen.getByText("C:/old-worktrees/review")).toBeTruthy();
  expect(screen.getByText("History reported · 1 turn · Event 4")).toBeTruthy();
  expect(screen.getByText(/Native context contents are unconfirmed/)).toBeTruthy();
  expect(screen.getByText("Requested skill shared")).toBeTruthy();
  expect(screen.getByText("CLI reported skill availability")).toBeTruthy();
  expect(screen.getByText("shared · Use unconfirmed")).toBeTruthy();
  expect(screen.getByText("shared · Use reported")).toBeTruthy();
  const reports = within(screen.getByRole("list", { name: "Provider inventory observations" }));
  const linkedGuide = reports.getByRole<HTMLAnchorElement>("link", { name: guide });
  expect(document.getElementById(linkedGuide.getAttribute("href")?.slice(1) ?? "")?.textContent).toContain("guide-r1");
  expect(reports.getByText("Saved identity unresolved")).toBeTruthy();

  view.rerender(attempt("run-b"));
  await user.click(screen.getByRole("button", { name: "Dispatch inventory" }));
  expect(await screen.findByText("Recorded revision guide-r2")).toBeTruthy();
  expect(screen.getByText("workflow · Version 2.0.0")).toBeTruthy();
  expect(screen.getByText("project · Disabled")).toBeTruthy();

  view.rerender(attempt("run-a"));
  await user.click(screen.getByRole("button", { name: "Dispatch inventory" }));
  expect(await screen.findByText("Recorded revision guide-r1")).toBeTruthy();
  expect(screen.getByText("workflow · Version 1.2.0")).toBeTruthy();
  expect(screen.getByText("project · Enabled")).toBeTruthy();
  expect(screen.getByText("discovered-only · Use unconfirmed")).toBeTruthy();
});
