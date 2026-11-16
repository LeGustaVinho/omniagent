// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import type { Job } from "@/bindings/Job";
import { defaultConfiguration, type WorkflowNodeConfiguration } from "@/workflow-graph-model";
import { WorkflowMcpNodeConfiguration, WorkflowMcpToolRun } from "@/workflow-mcp-tools";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const server = { id: "server", revision: "server-r1", name: "Fixture", executable: "C:/fixture/tool.exe", tools: [
  { name: "update", revision: "contract-r1", inputSchema: { type: "object", required: ["destination", "priority"], properties: {
    destination: { type: "string" }, priority: { type: "integer", minimum: 0 }, toggle: { type: "boolean" },
    route: { enum: ["direct", 5] }, options: { type: "object" },
  } }, outputSchema: { type: "object" } },
  { name: "check", revision: "contract-other", inputSchema: { type: "object", properties: { enabled: { type: "boolean" } } }, outputSchema: null },
] };
function provider() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return function Provider({ children }: { children: React.ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

it("builds typed, required, and mapped fields from the selected pinned tool schema", async () => {
  vi.mocked(invoke).mockImplementation(async (name) => {
    if (name === "workflow_mcp_servers") return [server];
    throw Error(`Unexpected command ${name}`);
  });
  function Editor() {
    const [saved, setSaved] = useState<WorkflowNodeConfiguration>(defaultConfiguration("mcp_tool"));
    return <><WorkflowMcpNodeConfiguration projectId="project" configuration={saved} disabled={false} onChange={setSaved} />
      <output aria-label="Recorded configuration">{JSON.stringify(saved)}</output></>;
  }
  render(<Editor />, { wrapper: provider() });
  const user = userEvent.setup();
  await screen.findByRole("option", { name: "Fixture" });
  await user.selectOptions(screen.getByRole("combobox", { name: "Local MCP server" }), "server");
  await user.selectOptions(screen.getByRole("combobox", { name: "Tool" }), "update");
  const destination = screen.getByRole<HTMLInputElement>("textbox", { name: "destination" });
  const priority = screen.getByRole<HTMLInputElement>("spinbutton", { name: "priority" });
  expect(destination.required).toBe(true);
  expect(priority.required).toBe(true);
  expect(priority.step).toBe("1");
  expect(screen.queryByRole("combobox", { name: "enabled" })).toBeNull();
  await user.click(screen.getByText("Argument JSON"));
  const fixedJson = screen.getByRole("textbox", { name: "Fixed arguments JSON" });
  await user.clear(fixedJson);
  await user.paste('{"options":{"quiet":true},"extra":[1,2]}');
  await user.type(destination, "initial");
  await user.type(priority, "0");
  await user.selectOptions(screen.getByRole("combobox", { name: "toggle" }), "1");
  await user.selectOptions(screen.getByRole("combobox", { name: "route" }), "1");
  const current = () => JSON.parse(screen.getByRole("status", { name: "Recorded configuration" }).textContent ?? "{}");
  expect(current()).toMatchObject({ serverRevision: "server-r1", contractRevision: "contract-r1",
    arguments: { destination: "initial", priority: 0, toggle: false, route: 5, options: { quiet: true }, extra: [1, 2] },
  });
  await user.selectOptions(screen.getByRole("combobox", { name: "destination source" }), "artifact");
  expect(screen.getByRole("alert").textContent).toBe("Enter a JSON Pointer for destination.");
  const pointer = screen.getByRole<HTMLInputElement>("textbox", { name: "destination JSON Pointer" });
  expect(pointer.required).toBe(true);
  await user.type(pointer, "/evidence/path");
  expect(screen.queryByRole("alert")).toBeNull();
  expect(current().arguments).not.toHaveProperty("destination");
  expect(current().argumentMappings).toEqual({ destination: "/evidence/path" });
  await user.click(screen.getByText("Pinned schemas"));
  expect(screen.getByText(/"required"/)).toBeTruthy();
});

it("asks the operator to approve the exact destination and reconciles an uncertain effect without replay", async () => {
  const ids = { projectId: "project", runId: "workflow-run", checkoutId: "checkout", nodeId: "tool" };
  const pending = { id: "tool-job", queueState: "paused", latestAttempt: null } as Job;
  let reconciled = false;
  vi.mocked(invoke).mockImplementation(async (name) => {
    if (name === "submit_workflow_mcp_tool") return pending;
    if (name === "workflow_mcp_job_review") return { serverName: "Fixture", toolName: "update", destination: "sandbox-only",
      arguments: { destination: "sandbox-only" }, decision: "pending" };
    if (name === "decide_workflow_mcp_job") return null;
    if (name === "workflow_mcp_attempt_evidence") return { state: reconciled ? "reconciled" : "uncertain", artifact: null };
    if (name === "reconcile_workflow_mcp_attempt") { reconciled = true; return null; }
    throw Error(`Unexpected command ${name}`);
  });
  const { rerender } = render(<WorkflowMcpToolRun {...ids} waitsForArguments />, { wrapper: provider() });
  const user = userEvent.setup();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Prepare tool call" }).disabled).toBe(true);
  rerender(<WorkflowMcpToolRun {...ids} argumentsArtifact="selected-artifact" />);
  await user.click(screen.getByRole("button", { name: "Prepare tool call" }));
  expect(invoke).toHaveBeenCalledWith("submit_workflow_mcp_tool", { submission: { ...ids, inputs: { arguments: "selected-artifact" } } });
  rerender(<WorkflowMcpToolRun {...ids} argumentsArtifact="selected-artifact" job={pending} />);
  expect(await screen.findByText("sandbox-only")).toBeTruthy();
  await user.click(await screen.findByRole("button", { name: "Approve tool call" }));
  expect(invoke).toHaveBeenCalledWith("decide_workflow_mcp_job", { projectId: ids.projectId, jobId: pending.id, approve: true });
  rerender(<WorkflowMcpToolRun {...ids} argumentsArtifact="selected-artifact" job={{ ...pending,
    latestAttempt: { runId: "tool-run", status: "interrupted" } as NonNullable<Job["latestAttempt"]>,
  }} />);
  expect(await screen.findByText("External effect needs review")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Record external effect reviewed" }));
  expect(invoke).toHaveBeenCalledWith("reconcile_workflow_mcp_attempt", { projectId: ids.projectId, runId: "tool-run" });
  expect(await screen.findByText("reconciled")).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.filter(([name]) => name === "submit_workflow_mcp_tool")).toHaveLength(1);
});
