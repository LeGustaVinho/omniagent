// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ConversationSubagentQuota } from "@/bindings/ConversationSubagentQuota";
import { ConversationSubagentControls, ConversationSubagentQuotaPanel } from "@/subagent-quota";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

it("shares the saved limit with the inspector and refreshes managed and reported native counters separately", async () => {
  const child = { conversationId: "child-conversation", runId: "child-attempt" };
  const root = "originating-conversation";
  let snapshot: ConversationSubagentQuota = {
    conversationId: root, project: { mode: "strict", maximum: 4 },
    modeOverride: null, requested: 3, authorized: 3, nativeRequested: 1,
    nativeScope: { runId: child.runId, provider: "codex", allowance: 1, enforcement: "pending" },
    nativeSessions: [], native: [], configurationPending: false,
    managed: { active: 0, queued: 1, historical: 0 },
  };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "conversation_subagent_quota":
        expect(args).toEqual(child);
        return snapshot;
      case "conversation_subagent_audit":
        return { projectDefault: "available", conversationOverride: null };
      case "set_conversation_subagent_quota":
        expect(args).toEqual({ conversationId: root, requested: 6, nativeRequested: 2, modeOverride: "strict" });
        snapshot = { ...snapshot, requested: 6, authorized: 4, nativeRequested: 2, modeOverride: "strict", configurationPending: true };
        return {
          ...snapshot,
          nativeScope: { runId: "root-attempt", provider: "codex", allowance: 2, enforcement: "enforced" },
        };
      default: throw new Error(`Unexpected command ${command}`);
    }
  });

  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}>
    <ConversationSubagentControls {...child} />
    <ConversationSubagentQuotaPanel {...child} live />
  </QueryClientProvider>);
  const user = userEvent.setup();
  const panel = within(screen.getByRole("region", { name: "Subagent limits" }));
  const authorized = () => panel.getByText("Authorized now").nextElementSibling?.textContent;
  await panel.findByText("0 active · 1 queued · 0 historical");
  expect(authorized()).toBe("3");
  expect(panel.getByText("Codex · Waiting for the CLI to verify the reserved cap of 1")).toBeTruthy();

  await user.click(screen.getByRole("button", { name: "Subagent limits" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Subagent limits" }));
  expect(dialog.getByText("Project maximum").nextElementSibling?.textContent).toBe("4");
  expect(dialog.getByText("Authorized now").nextElementSibling?.textContent).toBe("3");
  const audit = dialog.getByText("Audit policy").closest("details")!;
  const activity = dialog.getByText("Activity and CLI limits").closest("details")!;
  expect(audit.open).toBe(false);
  expect(activity.open).toBe(false);
  await user.click(dialog.getByText("Audit policy"));
  expect(await dialog.findByRole("combobox", { name: "Subagent audit policy" })).toBeTruthy();
  await user.click(dialog.getByText("Audit policy"));
  await user.click(dialog.getByText("Activity and CLI limits"));
  expect(dialog.getByRole("region", { name: "CLI delegation cap" })).toBeTruthy();
  await user.click(dialog.getByText("Activity and CLI limits"));
  await user.selectOptions(dialog.getByRole("combobox", { name: "Limit mode" }), "strict");
  const requested = dialog.getByRole("spinbutton", { name: "Requested limit" });
  await user.clear(requested);
  await user.type(requested, "6");
  expect(dialog.getByText("This request exceeds the project maximum of 4.")).toBeTruthy();
  const nativeRequested = dialog.getByRole("spinbutton", { name: /requested native allowance/i });
  await user.clear(nativeRequested);
  await user.type(nativeRequested, "2");
  await user.click(dialog.getByRole("button", { name: "Save limit" }));
  await dialog.findByText("Subagent policy changes are pending. Existing sessions keep their allowances until they stop.");
  await waitFor(() => expect(dialog.getByRole<HTMLButtonElement>("button", { name: "Close" }).disabled).toBe(false));
  await user.click(dialog.getByRole("button", { name: "Close" }));
  expect(authorized()).toBe("4");
  expect(panel.getByText("Configured limit mode").nextElementSibling?.textContent).toBe("Strict total limit");
  expect(panel.getByText("Codex · Waiting for the CLI to verify the reserved cap of 1")).toBeTruthy();
  expect(panel.queryByText("Codex · CLI cap verified · 2 allowed")).toBeNull();

  snapshot = {
    ...snapshot, configurationPending: false,
    managed: { active: 1, queued: 0, historical: 0 },
    nativeScope: { runId: child.runId, provider: "codex", allowance: 1, enforcement: "enforced" },
    native: [{ provider: "codex", reported: { active: 0, queued: 1, historical: 1 }, unknownState: 1, unidentifiedEvents: 1 }],
  };
  await panel.findByText("1 active · 0 queued · 0 historical", {}, { timeout: 2500 });
  expect(panel.getByText("Codex · CLI cap verified · 1 allowed")).toBeTruthy();
  const reported = within(panel.getByRole("region", { name: "Provider-reported subagents" }));
  expect(reported.getByText("0 active · 1 queued · 1 historical")).toBeTruthy();
  expect(reported.getByText("1 reported with unknown status.")).toBeTruthy();

  snapshot = {
    ...snapshot, managed: { active: 0, queued: 0, historical: 1 },
    nativeScope: { runId: child.runId, provider: "codex", allowance: 1, enforcement: "ended" },
  };
  await panel.findByText("0 active · 0 queued · 1 historical", {}, { timeout: 2500 });
  expect(panel.getByText("Codex · Attempt ended · 1 reserved at launch")).toBeTruthy();
  expect(authorized()).toBe("4");
}, 10000);
