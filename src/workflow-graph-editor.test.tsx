// @vitest-environment jsdom

import { useState, type ComponentType } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import PipelinesWorkspace from "@/pipelines-workspace";
import { WorkflowGraphEditor, type WorkflowEditorState } from "@/workflow-graph-editor";
import { defaultConfiguration, graphProblems, type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowGraph, type WorkflowPort } from "@/workflow-graph-model";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

vi.mock("@xyflow/react", async (importOriginal) => {
  type Position = { x: number; y: number };
  type Card = { id: string; position: Position; data: { definition: WorkflowDefinition } };
  const update = () => undefined;
  return {
    ...await importOriginal<typeof import("@xyflow/react")>(),
    Background: () => null,
    Controls: () => null,
    Handle: ({ id, type }: { id: string; type: string }) => <span data-port-handle={`${type}:${id}`} />,
    useUpdateNodeInternals: () => update,
    ReactFlow: ({ nodes, nodeTypes, onNodesChange }: {
      nodes: Card[]; nodeTypes: { workflow: ComponentType<Card> };
      onNodesChange: (changes: { type: "position"; id: string; position: Position }[]) => void;
    }) => <div>{nodes.map(({ id, ...card }) => <div key={id} data-testid={`node-${id}`} data-position={JSON.stringify(card.position)}>
      <nodeTypes.workflow id={id} {...card} />
      <button type="button" onClick={() => onNodesChange([{ type: "position", id, position: { x: 91, y: -8 } }])}>Move {id}</button>
    </div>)}</div>,
  };
});

afterEach(() => { cleanup(); vi.resetAllMocks(); });

const control = (name: string): WorkflowPort => ({ name, value: { kind: "control" } });
const data = (name: string, artifact: string): WorkflowPort => ({ name, value: { kind: "data", artifact } });
const start: WorkflowDefinition = { kind: "start", title: "Start", revision: 2, configuration: "none", executor: "bind_input", effect: "none",
  inputs: [], outputs: [control("start"), data("document", "markdown")] };
const old = { ...start, revision: 1, outputs: [control("start"), data("old-document", "markdown")] };
const context: WorkflowDefinition = { kind: "load_context", title: "Load context", revision: 1, configuration: "none", executor: "read_context", effect: "read_evidence",
  inputs: [control("ready"), data("document", "markdown")], outputs: [control("loaded"), data("context", "markdown")] };
const plan: WorkflowDefinition = { kind: "plan", title: "Plan", revision: 1, configuration: "agent_role", executor: "plan_agent", effect: "invoke_model",
  inputs: [control("ready"), data("context", "markdown")], outputs: [control("planned"), data("plan", "plan")] };
const wire = (from: string, output: string, to: string, input: string) => ({ from: { node: from, port: output }, to: { node: to, port: input } });

it("opens a protected draft, reviews an affected port upgrade, wires typed nodes, and restores the saved graph and layout", async () => {
  const graph: WorkflowGraph = {
    name: "Versioned review",
    nodes: [{ id: "start", kind: "start", configuration: { kind: "none" } }, { id: "context", kind: "load_context", configuration: { kind: "none" } }],
    wires: [wire("start", "start", "context", "ready"), wire("start", "old-document", "context", "document")],
  };
  const original = {
    id: "draft-1", conversationId: "chat-1", base: null,
    head: { id: "first-revision", runId: null, graph, layout: {}, definitions: { start: old, context } },
    request: { jobId: "job-1", runId: null, state: "saved", diagnostic: null },
  };
  let stored = original;
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    switch (command) {
      case "project_workflow_graph_drafts":
        expect(input).toEqual({ projectId: exampleCheckout.projectId });
        return [{ id: stored.id, name: stored.head.graph.name, revision: stored.head.id }];
      case "inspect_workflow_authoring": return stored;
      case "workflow_graph_registry": return [start, context, plan];
      case "conversation_models": return [];
      case "save_workflow_authoring_draft": {
        const args = input as { graph: WorkflowGraph; canvas: WorkflowCanvasEdit };
        stored = { ...original, head: { ...original.head, id: "saved-revision", graph: args.graph, layout: args.canvas.layout,
          definitions: { start, context, plan } as typeof original.head.definitions } };
        return stored;
      }
      default: throw new Error(`Unexpected ${command}`);
    }
  });
  const props = { checkout: exampleCheckout, simulation: null, startSimulation: vi.fn(),
    openWorkspace: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn() };
  const client = () => new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const mount = (target: string) => render(<QueryClientProvider client={client()}><PipelinesWorkspace {...props} target={target} /></QueryClientProvider>);
  const user = userEvent.setup();
  const list = mount("drafts");
  await user.click(screen.getByRole("button", { name: "Repository delivery" }));
  expect(props.openWorkspace).toHaveBeenCalledWith("pipelines", "delivery");
  await user.click(await screen.findByRole("button", { name: "Open draft" }));
  expect(props.openWorkspace).toHaveBeenCalledWith("pipelines", "authoring:draft-1");
  list.unmount();
  const first = mount("authoring:draft-1");
  const canvas = within(await screen.findByLabelText("Pipeline graph canvas"));
  expect(canvas.getByTestId("node-start").querySelector('[data-port-handle="source:old-document"]')).toBeTruthy();
  const inspector = within(screen.getByRole("complementary", { name: "Node configuration" }));
  const review = inspector.getByText("Review version 2");
  await user.click(review);
  expect(within(review.parentElement!).getByText("start.old-document → context.document")).toBeTruthy();
  await user.click(inspector.getByRole("button", { name: "Update to version 2" }));
  expect(screen.getByText("Output start.old-document is unavailable.")).toBeTruthy();
  const save = () => screen.getByRole<HTMLButtonElement>("button", { name: "Save draft revision" });
  expect(save().disabled).toBe(true);
  await user.click(inspector.getByRole("button", { name: "Remove start.old-document → context.document" }));
  await user.selectOptions(inspector.getByRole("combobox", { name: "Inspect node" }), "context");
  const connect = async (source: string, output: string, input: string) => {
    await user.selectOptions(inspector.getByRole("combobox", { name: "Source node" }), source);
    await user.selectOptions(inspector.getByRole("combobox", { name: "Output port" }), output);
    await user.selectOptions(inspector.getByRole("combobox", { name: "Input port" }), input);
    await user.click(inspector.getByRole("button", { name: "Connect ports" }));
  };
  await connect("start", "document", "document");
  await user.click(screen.getByRole("button", { name: "Add node" }));
  const catalog = within(screen.getByRole("region", { name: "Node catalog" }));
  await user.type(catalog.getByRole("searchbox"), "plan");
  expect(catalog.queryByRole("button", { name: "Load context" })).toBeNull();
  await user.click(catalog.getByRole("button", { name: "Plan" }));
  expect(screen.getByText(/Agent role is required/)).toBeTruthy();
  await user.type(inspector.getByRole("textbox", { name: "Agent role" }), "Qualified planner");
  await connect("context", "context", "ready");
  expect(screen.getByRole("alert").textContent).toContain("(markdown) cannot connect to plan.ready (Control)");
  await connect("context", "loaded", "ready");
  await connect("context", "context", "context");
  await user.click(canvas.getByRole("button", { name: "Move start" }));
  expect(save().disabled).toBe(false);
  await user.click(save());
  await waitFor(() => expect(screen.getByText("saved-revision")).toBeTruthy());
  expect(invoke).toHaveBeenCalledWith("save_workflow_authoring_draft", {
    projectId: exampleCheckout.projectId, draftId: "draft-1", selectedRevision: "first-revision",
    graph: { ...graph, nodes: [...graph.nodes, { id: "plan", kind: "plan", configuration: { kind: "agent_role", role: "Qualified planner" } }],
      wires: [wire("start", "start", "context", "ready"), wire("start", "document", "context", "document"),
        wire("context", "loaded", "plan", "ready"), wire("context", "context", "plan", "context")] },
    canvas: { layout: { start: { x: 91, y: -8 } }, updateDefinitions: { start: 2 } },
  });
  first.unmount();
  mount("authoring:draft-1");
  const reloaded = await screen.findByTestId("node-start");
  expect(reloaded.getAttribute("data-position")).toBe(JSON.stringify({ x: 91, y: -8 }));
  expect(reloaded.querySelector('[data-port-handle="source:document"]')).toBeTruthy();
  expect(save().disabled).toBe(true);
});

it("configures a typed list and a bounded iteration in the package graph", async () => {
  const list: WorkflowDefinition = {
    kind: "build_list", title: "Build list", revision: 1, configuration: "build_list", executor: "build_list", effect: "read_evidence",
    inputs: [control("ready"), { name: "value", value: { kind: "evidence" } }], outputs: [control("completed"), data("items", "json_list")],
  };
  const iteration: WorkflowDefinition = {
    kind: "for_each", title: "For each", revision: 1, configuration: "for_each", executor: "expand_for_each", effect: "none",
    inputs: [control("ready"), data("items", "json_list")], outputs: [control("completed"), data("results", "iteration_results")],
  };
  const catalog = [start, list, iteration, { ...iteration, kind: "for_each_begin", title: "Pin iteration list" }];
  let edited: WorkflowGraph = { name: "Iteration", nodes: [
    { id: "start", kind: "start", configuration: { kind: "none" } },
    { id: "list", kind: "build_list", configuration: defaultConfiguration("build_list") },
  ], wires: [wire("start", "start", "list", "ready")] };
  function Editor() {
    const [state, setState] = useState<WorkflowEditorState>({ graph: edited, layout: {}, updateDefinitions: {} });
    edited = state.graph;
    const definitions = Object.fromEntries(edited.nodes.map((node) => [node.id, catalog.find(({ kind }) => kind === node.kind)!]));
    return <WorkflowGraphEditor projectId="project" state={state} setState={setState} definitions={definitions} catalog={catalog}
      problems={graphProblems(edited, definitions)} disabled={false} allowDefinitionUpdates={false} />;
  }

  render(<Editor />);
  const user = userEvent.setup();
  const inspector = within(screen.getByRole("complementary", { name: "Node configuration" }));
  await user.selectOptions(inspector.getByRole("combobox", { name: "Inspect node" }), "list");
  expect(inspector.getByRole<HTMLTextAreaElement>("textbox", { name: "Items (JSON array)" }).value).toBe("[]");
  fireEvent.change(inspector.getByRole("textbox", { name: "Items (JSON array)" }), { target: { value: "[0, false]" } });
  expect(edited.nodes.find(({ id }) => id === "list")?.configuration).toEqual({ kind: "build_list", source: "values", values: [0, false] });
  await user.selectOptions(inspector.getByRole("combobox", { name: "List source" }), "evidence");
  fireEvent.change(inspector.getByRole("textbox", { name: /Array pointer/ }), { target: { value: "/evidence/items" } });
  expect(edited.nodes.find(({ id }) => id === "list")?.configuration).toEqual({ kind: "build_list", source: "evidence", pointer: "/evidence/items" });
  expect(screen.getByText(/list.value requires a connection/)).toBeTruthy();

  await user.click(screen.getByRole("button", { name: "Add node" }));
  const nodes = within(screen.getByRole("region", { name: "Node catalog" }));
  expect(nodes.queryByRole("button", { name: "Pin iteration list" })).toBeNull();
  await user.click(nodes.getByRole("button", { name: "For each" }));
  fireEvent.change(inspector.getByRole("spinbutton", { name: "Maximum items" }), { target: { value: "12" } });
  fireEvent.change(inspector.getByRole("textbox", { name: "Subflow result output" }), { target: { value: "check" } });
  await user.selectOptions(inspector.getByRole("combobox", { name: "Source node" }), "list");
  await user.selectOptions(inspector.getByRole("combobox", { name: "Output port" }), "items");
  await user.selectOptions(inspector.getByRole("combobox", { name: "Input port" }), "items");
  await user.click(inspector.getByRole("button", { name: "Connect ports" }));
  expect(edited.nodes.find(({ id }) => id === "for-each")?.configuration).toEqual({
    kind: "for_each", maxItems: 12, readyInput: "ready", itemInput: "item", completionOutput: "completed", resultOutput: "check",
  });
  expect(edited.wires).toContainEqual(wire("list", "items", "for-each", "items"));
});
