// @vitest-environment jsdom

import { useState } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { CommandNodeConfiguration, WorkflowCommandRun, type CommandNodeConfigurationValue } from "@/workflow-command";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const ipc = vi.mocked(invoke);

function provider() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return function TestProvider({ children }: { children: React.ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

beforeEach(() => { ipc.mockReset(); });
afterEach(cleanup);

it("registers a project-local script and saves its exact revision with a structured command input", async () => {
  const registration = {
    id: "command", revision: "c".repeat(64), name: "Build",
    target: { kind: "script", path: "C:\\Scripts\\build.ps1", executable: "C:\\Tools\\powershell.exe", interpreterArguments: ["-File"] },
  };
  ipc.mockImplementation(async (name) => {
    if (name === "list_workflow_command_registrations") return [];
    if (name === "register_workflow_command") return registration;
    throw new Error(name);
  });
  function Configuration() {
    const [configuration, setConfiguration] = useState<CommandNodeConfigurationValue>({
      kind: "command", registrationId: "", registrationRevision: "", arguments: [], input: {}, output: "text",
    });
    return <>
      <CommandNodeConfiguration projectId="project" configuration={configuration} onChange={setConfiguration} disabled={false} />
      <output aria-label="Saved configuration">{JSON.stringify(configuration)}</output>
    </>;
  }
  render(<Configuration />, { wrapper: provider() });
  fireEvent.click(screen.getByRole("button", { name: "Register local command" }));
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Build" } });
  fireEvent.change(screen.getByLabelText("Command type"), { target: { value: "script" } });
  fireEvent.change(screen.getByLabelText("Absolute script path"), { target: { value: registration.target.path } });
  fireEvent.change(screen.getByLabelText("Absolute interpreter path"), { target: { value: registration.target.executable } });
  fireEvent.change(screen.getByLabelText("Interpreter arguments before script, one per line"), { target: { value: "-File" } });
  fireEvent.click(screen.getByRole("button", { name: "Save command registration" }));
  await screen.findByRole("option", { name: "Build" });
  expect(ipc).toHaveBeenCalledWith("register_workflow_command", {
    projectId: "project", request: { name: "Build", target: registration.target },
  });
  fireEvent.change(screen.getByLabelText("Arguments, one per line"), { target: { value: "first argument\nsecond" } });
  fireEvent.change(screen.getByLabelText("Standard input JSON"), { target: { value: '{"labels":["build","test"]}' } });
  fireEvent.change(screen.getByLabelText("Standard output"), { target: { value: "json" } });
  expect(JSON.parse(screen.getByLabelText("Saved configuration").textContent ?? "")).toEqual({
    kind: "command", registrationId: "command", registrationRevision: registration.revision,
    arguments: ["first argument", "second"], input: { labels: ["build", "test"] }, output: "json",
  });
  fireEvent.change(screen.getByLabelText("Standard input JSON"), { target: { value: "[]" } });
  expect(screen.getByRole("alert").textContent).toContain("Enter a JSON object");
});

it("queues a released command and renders its verified result independently of stderr and process status", async () => {
  ipc.mockImplementation(async (name) => {
    if (name === "submit_workflow_command") return { id: "job", latestAttempt: null };
    if (name === "workflow_command_result") return {
      value: { kind: "command_result", evidence: { runId: "attempt", output: { format: "json", value: { count: 2 } } } },
    };
    if (name === "command_job_output") return {
      stdout: '{"count":2}', stderr: "build log", truncated: false, process: { code: 0, cancelled: false },
    };
    throw new Error(name);
  });
  const scope = { projectId: "project", runId: "workflow", nodeId: "build" };
  const onQueued = vi.fn();
  const { rerender } = render(<WorkflowCommandRun {...scope} onQueued={onQueued} />, { wrapper: provider() });
  expect((screen.getByRole("button", { name: "Run command" }) as HTMLButtonElement).disabled).toBe(true);
  rerender(<WorkflowCommandRun {...scope} releaseId="release-artifact" worktreeId="tree-artifact" onQueued={onQueued} />);
  fireEvent.click(screen.getByRole("button", { name: "Run command" }));
  await waitFor(() => expect(onQueued).toHaveBeenCalled());
  expect(ipc).toHaveBeenCalledWith("submit_workflow_command", {
    submission: { ...scope, inputs: { release: "release-artifact", worktree: "tree-artifact" } },
  });
  rerender(<WorkflowCommandRun {...scope} releaseId="release-artifact" worktreeId="tree-artifact"
    job={{ id: "job", latestAttempt: { runId: "attempt", status: "succeeded" } }} />);
  expect((await screen.findByLabelText("Command result")).textContent).toContain('"count": 2');
  expect(await screen.findByText("build log")).toBeTruthy();
  expect(screen.getByText("Verified exit code")).toBeTruthy();
});
