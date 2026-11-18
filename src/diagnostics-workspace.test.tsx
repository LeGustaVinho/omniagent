// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { RealDiagnostics } from "@/desktop-dock";
import { downloadArtifact } from "@/download-artifact";
import { FixtureRunPanel } from "@/fixture-panels";
import { exampleCheckout } from "@/workbench-contract";
import { useWorkspace } from "@/workspace-state";

const { getVersion, invoke } = vi.hoisted(() => ({ getVersion: vi.fn(), invoke: vi.fn() }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@/download-artifact", () => ({ downloadArtifact: vi.fn() }));

afterEach(() => {
  cleanup();
  getVersion.mockReset();
  invoke.mockReset();
  vi.mocked(downloadArtifact).mockReset();
  useWorkspace.setState({ checkout: null });
});

it("reads actual desktop and CLI status and exposes the device settings link", async () => {
  getVersion.mockResolvedValue("0.1.0");
  invoke.mockImplementation(async (command: string) => {
    switch (command) {
      case "runtime_status": return "Desktop runtime is ready.";
      case "desktop_cli_prerequisites": return {
        git: { executable: "C:\\Git\\git.exe", version: "git version fixture", issue: null },
        providers: [],
      };
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const openSystem = vi.fn();
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <RealDiagnostics checkout={exampleCheckout} openSystem={openSystem} />
  </QueryClientProvider>);

  const desktop = screen.getByRole("region", { name: "Current installation" });
  expect(await within(desktop).findByText("Desktop runtime is ready.")).toBeTruthy();
  expect(await within(desktop).findByText("0.1.0")).toBeTruthy();
  const executables = screen.getByRole("region", { name: "Installed executables" });
  expect(await within(executables).findByText("git version fixture")).toBeTruthy();
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "System settings" }));
  expect(openSystem).toHaveBeenCalledOnce();
  fireEvent.contextMenu(within(desktop).getByText("Desktop runtime is ready."));
  await user.click(screen.getByRole("menuitem", { name: "Copy installation summary" }));
  expect(await navigator.clipboard.readText()).toBe("OmniAgent 0.1.0\nDesktop runtime is ready.");
  await user.click(screen.getByRole("button", { name: "Check installed executables" }));
  await waitFor(() => expect(invoke.mock.calls.filter(([command]) => command === "desktop_cli_prerequisites")).toHaveLength(2));
  expect(invoke.mock.calls.every(([command]) => ["runtime_status", "desktop_cli_prerequisites"].includes(command))).toBe(true);
});

it("copies only the selected fixture's events and keeps protected input display scoped to its run", async () => {
  useWorkspace.setState({ checkout: exampleCheckout });
  const runs = ["first-fixture", "second-fixture"].map((id) => ({
    id, projectId: exampleCheckout.projectId, checkoutId: exampleCheckout.checkoutId, turnId: id, status: "succeeded",
  }));
  invoke.mockImplementation(async (command: string, { runId } = { runId: "" }) => {
    if (command === "run_snapshot") return { cursor: "2", runs, recentEvents: runs.map(({ id, turnId }, index) => ({
      seq: String(index + 1), turnId, runId: id, data: { type: "output", stream: "stdout", preview: `Output for ${id}` },
    })) };
    if (command === "fixture_input_evidence") return { sent: true, text: `Protected input for ${runId}` };
    throw new Error(`Unexpected command ${command}`);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const panelFor = (runId: string) => <QueryClientProvider client={client}>
    <FixtureRunPanel {...({ params: { runId } } as Parameters<typeof FixtureRunPanel>[0])} />
  </QueryClientProvider>;
  const { rerender } = render(panelFor("first-fixture"));
  const user = userEvent.setup();
  const first = await screen.findByRole("region", { name: "Run first-fi" });
  await within(first).findByText("Output for first-fixture");
  fireEvent.contextMenu(within(first).getByRole("heading"));
  await user.click(screen.getByRole("menuitem", { name: "Copy recent fixture events" }));
  expect(await navigator.clipboard.readText()).toBe("[1 stdout] Output for first-fixture");
  expect(invoke.mock.calls.some(([command]) => command === "fixture_input_evidence")).toBe(false);
  await user.click(within(first).getByRole("button", { name: "Inspect protected input" }));
  expect(await within(first).findByText("Protected input for first-fixture")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("fixture_input_evidence", { checkoutId: exampleCheckout.checkoutId, runId: "first-fixture" });

  rerender(panelFor("second-fixture"));
  expect(await screen.findByText("Output for second-fixture")).toBeTruthy();
  expect(screen.queryByText("Protected input for first-fixture")).toBeNull();
  expect(invoke).not.toHaveBeenCalledWith("fixture_input_evidence", { checkoutId: exampleCheckout.checkoutId, runId: "second-fixture" });
});

it("filters fixture events by channel and exports the exact visible recorded previews with scope", async () => {
  useWorkspace.setState({ checkout: exampleCheckout });
  const { projectId, checkoutId } = exampleCheckout;
  const run = { id: "recorded-fixture", projectId, checkoutId, turnId: "recorded-turn", status: "failed" };
  const event = (seq: string, data: unknown, runId = run.id) => ({ seq, runId, turnId: run.turnId, data });
  const failure = event("9007199254740993192", { type: "output", stream: "stderr", preview: "  diagnostic [redacted]…" });
  const status = event("9007199254740993193", { type: "status", status: "failed" });
  const recentEvents = [
    event("9007199254740993191", { type: "output", stream: "stdout", preview: "Output from selected run" }),
    failure,
    status,
    event("9007199254740993194", { type: "output", stream: "stderr", preview: "Unrelated run text" }, "other"),
  ];
  invoke.mockImplementation(async (command: string) => {
    if (command === "run_snapshot") return { cursor: recentEvents.at(-1)?.seq, runs: [run], recentEvents };
    throw new Error(`Unexpected command ${command}`);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>
    <FixtureRunPanel {...({ params: { runId: run.id } } as Parameters<typeof FixtureRunPanel>[0])} />
  </QueryClientProvider>);
  const fixture = within(await screen.findByRole("region", { name: "Run recorded" }));
  const events = within(await fixture.findByRole("list", { name: "Run events" }));
  const preview = events.getByText("diagnostic [redacted]…");
  const user = userEvent.setup();
  fireEvent.contextMenu(preview);
  await user.click(screen.getByRole("menuitem", { name: "Copy event as JSON" }));
  expect(JSON.parse(await navigator.clipboard.readText())).toEqual(failure);

  fireEvent.contextMenu(preview);
  await user.click(screen.getByRole("menuitem", { name: "Show only stderr events" }));
  expect((fixture.getByRole("combobox", { name: "Fixture event filter" }) as HTMLSelectElement).value).toBe("stderr");
  expect(events.getAllByRole("listitem")).toHaveLength(1);
  expect(events.queryByText("Unrelated run text")).toBeNull();
  expect(fixture.getByText(/Output previews may be redacted or shortened/)).toBeTruthy();
  fireEvent.contextMenu(fixture.getByRole("heading"));
  await user.click(screen.getByRole("menuitem", { name: "Copy visible fixture events as JSON" }));
  const expected = { source: "fixture_recent_events", checkoutId, runId: run.id, filter: "stderr", output: "redacted_preview", events: [failure] };
  expect(JSON.parse(await navigator.clipboard.readText())).toEqual(expected);
  fireEvent.contextMenu(fixture.getByRole("heading"));
  await user.click(screen.getByRole("menuitem", { name: "Download visible fixture events (.json)" }));
  const [name, download] = vi.mocked(downloadArtifact).mock.calls[0]!;
  expect(name).toBe("fixture-recorded-fixture-events-stderr.json");
  expect(download.type).toBe("application/json");
  const fileText = await new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = reject;
    reader.readAsText(download);
  });
  expect(JSON.parse(fileText)).toEqual(expected);
  await user.selectOptions(fixture.getByRole("combobox", { name: "Fixture event filter" }), "status");
  expect(events.getAllByRole("listitem")).toHaveLength(1);
  expect(events.getByText("Failed")).toBeTruthy();
  await user.selectOptions(fixture.getByRole("combobox", { name: "Fixture event filter" }), "stdout");
  expect(events.getAllByRole("listitem")).toHaveLength(1);
  expect(events.getByText("Output from selected run")).toBeTruthy();
  expect(invoke.mock.calls.every(([command]) => command === "run_snapshot")).toBe(true);
});
