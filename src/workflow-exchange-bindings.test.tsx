// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { WorkflowExchangeBindings, type ExchangeBindingRequirement } from "@/workflow-exchange-bindings";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
afterEach(() => { cleanup(); invoke.mockReset(); });

function mount(requirements: Record<string, ExchangeBindingRequirement>) {
  const onChange = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>
    <WorkflowExchangeBindings projectId="destination" requirements={requirements} disabled={false} onChange={onChange} />
  </QueryClientProvider>);
  return onChange;
}

it("pins the project command revision and clears the binding when args or input stop matching", async () => {
  invoke.mockResolvedValue([{ id: "local", revision: "local-revision", name: "Local verifier" }]);
  const change = mount({ verify: {
    name: "Verify", member: "package-workflow", node: "verification", requiredBeforeImport: true,
    kind: { type: "command", argumentCount: 2, inputFields: ["path"] },
  } });
  const registration = await screen.findByRole<HTMLOptionElement>("option", { name: "Local verifier" });
  fireEvent.change(screen.getByLabelText("Local command registration"), { target: { value: registration.value } });
  const args = screen.getByLabelText("Command arguments (JSON array)");
  const input = screen.getByLabelText("Command input (JSON object)");
  fireEvent.change(args, { target: { value: '["with spaces",""]' } });
  fireEvent.change(input, { target: { value: '{"path":"src/test.rs"}' } });
  expect(change).toHaveBeenLastCalledWith("verify", {
    type: "command", registrationId: "local", registrationRevision: "local-revision",
    arguments: ["with spaces", ""], input: { path: "src/test.rs" },
  });
  fireEvent.change(input, { target: { value: '{"path":"src/test.rs","extra":false}' } });
  expect(change).toHaveBeenLastCalledWith("verify", undefined);
  fireEvent.change(input, { target: { value: '{"path":9007199254740993}' } });
  expect(change).toHaveBeenLastCalledWith("verify", undefined);
  fireEvent.change(input, { target: { value: '{"path":"src/other.rs"}' } });
  fireEvent.change(args, { target: { value: '["too few"]' } });
  expect(change).toHaveBeenLastCalledWith("verify", undefined);
  expect(invoke).toHaveBeenCalledWith("list_workflow_command_registrations", { projectId: "destination" });
});

it("only offers MCP servers pinned to the tool revision and leaves reusable requirements for their consumer", async () => {
  const kind = {
    type: "mcp_server" as const, tool: "write_record", contractRevision: "target-revision",
    inputSchema: { type: "object" }, outputSchema: null, argumentFields: ["destination"],
  };
  invoke.mockResolvedValue([
    { id: "server", revision: "revision-1", name: "Matching server", tools: [{ name: kind.tool, revision: kind.contractRevision }] },
    { id: "older", revision: "revision-2", name: "Stale contract", tools: [{ name: kind.tool, revision: "old-contract" }] },
    { id: "another", revision: "revision-3", name: "Different tool", tools: [{ name: "read_record", revision: kind.contractRevision }] },
  ]);
  const change = mount({
    update: { name: "Update", member: "graph", node: "write", requiredBeforeImport: true, kind },
    library: { name: "Reusable contract", member: "library", node: null, requiredBeforeImport: false, kind },
  });
  const server = await screen.findByRole<HTMLOptionElement>("option", { name: "Matching server" });
  expect(screen.queryByRole("option", { name: "Stale contract" })).toBeNull();
  expect(screen.queryByRole("option", { name: "Different tool" })).toBeNull();
  expect(screen.getByText(/Bind in the consuming workflow/)).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Local MCP server"), { target: { value: server.value } });
  const args = screen.getByLabelText("MCP arguments (JSON object)");
  fireEvent.change(args, { target: { value: '{"destination":"target"}' } });
  expect(change).toHaveBeenLastCalledWith("update", {
    type: "mcp_server", serverId: "server", serverRevision: "revision-1", arguments: { destination: "target" },
  });
  fireEvent.change(args, { target: { value: "[]" } });
  expect(change).toHaveBeenLastCalledWith("update", undefined);
  expect(change.mock.calls.every(([key]) => key === "update")).toBe(true);
});

it("uses the shared picker for only the original provider and model across local profiles", async () => {
  const selection = { provider: "codex", profileId: "local-profile", modelId: "plan-model" };
  const option = (name: string, candidate: object) => ({ option: { selection: candidate, name, selectable: true, rating: null } });
  invoke.mockResolvedValue([
    option("Verified local model", selection),
    option("Different model", { ...selection, modelId: "wrong-model" }),
    option("Different provider", { ...selection, provider: "claude" }),
  ]);
  const change = mount({ planner: {
    name: "Planner", member: "planner-role", node: null, requiredBeforeImport: true,
    kind: { type: "model_profile", provider: "codex", modelId: "plan-model" },
  } });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: /Local profile for codex.*Unbound/ }));
  expect(screen.queryByRole("menuitemradio", { name: "Different model" })).toBeNull();
  expect(screen.queryByRole("menuitemradio", { name: "Different provider" })).toBeNull();
  await user.click(screen.getByRole("menuitemradio", { name: "Verified local model" }));
  expect(change).toHaveBeenLastCalledWith("planner", { type: "model_profile", selection });
  expect(invoke).toHaveBeenCalledWith("conversation_models", { provider: "codex", modelId: "plan-model" });
});
