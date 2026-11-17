// @vitest-environment jsdom

import { invoke } from "@tauri-apps/api/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { QualificationMatrix } from "@/qualification-matrix";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("loads the recorded dispatch scope on demand and distinguishes a fixture from provider verification", async () => {
  const cursor = { recordedAt: 1776000000, runId: "recorded-dispatch" };
  const dispatch = {
    ...cursor, provider: "codex", cliVersion: "recorded-version", requestedModel: "selected-model", reportedModel: "recorded-model",
    platform: { host: { major: 10, minor: 0, build: 26100, revision: 753, nativeArchitecture: 9, server: false }, guest: null },
    runner: "windows-direct", mode: "read_only", validation: null,
    capabilities: [{
      subject: { kind: "protocol", capability: "side_request" }, boundary: "exact_checkout", status: "fixture_passed", observed: null,
      evidence: { origin: "authenticated_fixture", result: "succeeded", checkedAt: "2026-01-01T00:00:00Z" },
    }, {
      subject: { kind: "protocol", capability: "workspace_write" }, boundary: "equivalent_configuration", status: "reported", observed: null,
      evidence: null,
    }],
  };
  vi.mocked(invoke).mockResolvedValueOnce({ records: [dispatch], nextCursor: cursor })
    .mockResolvedValueOnce({ records: [], nextCursor: null });
  const user = userEvent.setup();
  const client = new QueryClient();
  render(<QueryClientProvider client={client}><QualificationMatrix /></QueryClientProvider>);
  expect(invoke).not.toHaveBeenCalled();
  await user.click(screen.getByText("Qualification matrix"));
  const table = await screen.findByRole("table", { name: "Qualification matrix" });
  expect(invoke).toHaveBeenCalledWith("desktop_capability_qualifications", { cursor: null, validationCheckoutId: null });
  expect(within(table).getByText("Windows 10.0.26100.753 · x64")).toBeTruthy();
  expect(within(table).getByText("recorded-model")).toBeTruthy();
  await user.click(within(table).getByText("Recorded capabilities"));
  expect(within(table).getByText("side request · Fixture passed")).toBeTruthy();
  expect(within(table).getByText("workspace write · Reported")).toBeTruthy();
  expect(within(table).queryByText(/Verified in provider run/)).toBeNull();
  fireEvent.contextMenu(within(table).getByText("side request · Fixture passed"));
  await user.click(screen.getByRole("menuitem", { name: "Copy capability assessment" }));
  expect(await navigator.clipboard.readText()).toBe("side request · Fixture passed\nExact checkout\nAuthenticated fixture · succeeded · 2026-01-01T00:00:00Z");
  fireEvent.contextMenu(within(table).getByText("Codex"));
  await user.click(screen.getByRole("menuitem", { name: "Copy qualification run ID" }));
  expect(await navigator.clipboard.readText()).toBe(cursor.runId);
  await user.click(screen.getByRole("button", { name: "Older" }));
  expect(await screen.findByText("No submitted dispatch profiles have been recorded.")).toBeTruthy();
  expect(invoke).toHaveBeenLastCalledWith("desktop_capability_qualifications", { cursor, validationCheckoutId: null });
});

it("uses the eligible attempt's recorded scope only after explicit validation", async () => {
  const target = {
    conversationId: "saved-source", sourceRunId: "saved-attempt", mode: "plan", effort: "medium",
    selection: { provider: "codex", profileId: "saved-profile", modelId: "requested-model" },
  };
  vi.mocked(invoke).mockResolvedValueOnce({ records: [{
    runId: target.sourceRunId, recordedAt: 1776000000, provider: "codex", cliVersion: "1",
    requestedModel: target.selection.modelId, reportedModel: "effective-model", platform: null,
    runner: "windows-direct", mode: target.mode, capabilities: [], validation: target,
  }], nextCursor: null }).mockResolvedValueOnce({ status: "skipped" });
  const user = userEvent.setup();
  const client = new QueryClient();
  render(<QueryClientProvider client={client}><QualificationMatrix validationCheckoutId="active-checkout" /></QueryClientProvider>);
  await user.click(screen.getByText("Qualification matrix"));
  await user.click(await screen.findByText("Validate capability"));
  const validation = await screen.findByRole("region", { name: "Side request capability validation" });
  expect(invoke).toHaveBeenCalledExactlyOnceWith("desktop_capability_qualifications", { cursor: null, validationCheckoutId: "active-checkout" });
  await user.click(within(validation).getByRole("button", { name: "Validate capability" }));
  expect(await within(validation).findByText(/eligible source turn changed/)).toBeTruthy();
  expect(invoke).toHaveBeenLastCalledWith("enqueue_conversation_side_request", {
    conversationId: target.conversationId, expectedSourceRunId: target.sourceRunId, expectedContextId: null,
    selection: target.selection, effort: target.effort, mode: target.mode, purpose: "validation", question: "",
  });
});

it("shows the same columns in Simulate without reading the real profile history", async () => {
  const user = userEvent.setup();
  render(<QualificationMatrix examples />);
  await user.click(screen.getByText("Qualification matrix"));
  const table = screen.getByRole("table", { name: "Qualification matrix" });
  expect(within(table).getAllByRole("columnheader").map((cell) => cell.textContent))
    .toEqual(["CLI", "Model", "Host", "Runner", "Qualification"]);
  expect(within(table).getByText("Hyper-V guest")).toBeTruthy();
  expect(invoke).not.toHaveBeenCalled();
});
