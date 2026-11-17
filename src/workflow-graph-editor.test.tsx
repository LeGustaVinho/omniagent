// @vitest-environment jsdom

import { useState, type ComponentType } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { PipelineCatalog } from "@/pipeline-catalog";
import PipelinesWorkspace from "@/pipelines-workspace";
import { WorkflowGraphEditor, type WorkflowEditorState } from "@/workflow-graph-editor";
import { configurationProblem, connectionProblem, defaultConfiguration, graphProblems, type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowGraph, type WorkflowPort } from "@/workflow-graph-model";
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

it("opens the real Library and searches the same definitions used by the Builder", async () => {
  const ipc = vi.mocked(invoke);
  ipc.mockImplementation(async (command) => {
    if (command === "workflow_graph_registry") return [start, context, plan];
    if (command === "workflow_package_catalog") return [];
    throw new Error(command);
  });
  const openWorkspace = vi.fn();
  const user = userEvent.setup();
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <PipelinesWorkspace checkout={exampleCheckout} simulation={null} target="library" openWorkspace={openWorkspace}
      startSimulation={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);
  const tabs = within(screen.getByRole("navigation", { name: "Pipelines views" }));
  expect(tabs.getByRole("button", { name: "Library" }).getAttribute("aria-current")).toBe("page");
  const library = within(screen.getByRole("region", { name: "Workflow library" }));
  await library.findByRole("heading", { name: "Load context" });
  expect(ipc).toHaveBeenCalledWith("workflow_graph_registry");
  const loadContext = library.getByRole("button", { name: "Load context" });
  loadContext.focus();
  await user.keyboard("{Enter}");
  const dialog = within(screen.getByRole("dialog", { name: "Load context" }));
  expect(dialog.getByRole("heading", { name: "How it works" })).toBeTruthy();
  expect(dialog.getByText("Prepare a planning input")).toBeTruthy();
  expect(dialog.getByText("No editable parameters for this definition.")).toBeTruthy();
  const outputs = within(dialog.getByRole("region", { name: "Outputs" }));
  expect(outputs.getByText("loaded")).toBeTruthy();
  expect(outputs.getByText("markdown")).toBeTruthy();
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(document.activeElement).toBe(loadContext);

  const category = library.getByRole("combobox", { name: "Category" });
  const effect = library.getByRole("combobox", { name: "Effect" });
  await user.selectOptions(category, "Entry and results");
  expect(library.getByRole("status").textContent).toBe("2 of 3 nodes");
  await user.selectOptions(effect, "Model calls");
  expect(library.getByText("No nodes match your filters.")).toBeTruthy();
  await user.click(library.getByRole("button", { name: "Clear filters" }));
  expect(library.getByRole("status").textContent).toBe("3 nodes");
  const search = library.getByRole("searchbox", { name: "Search pipeline nodes" });
  expect(document.activeElement).toBe(search);

  await user.type(search, "planned");
  expect(library.queryByRole("heading", { name: "Load context" })).toBeNull();
  expect(library.getByRole("heading", { name: "Plan" })).toBeTruthy();
  await user.click(tabs.getByRole("button", { name: "Maintenance" }));
  expect(openWorkspace).toHaveBeenCalledWith("pipelines", "maintenance");
});

it("reads a published preset's pinned ports and editable parameters only when opened", async () => {
  const version = { id: "review-preset", revision: "pinned-review-revision", kind: "preset", destination: "library" };
  const definition = { ...plan, inputs: [data("notes", "markdown")] };
  const ipc = vi.mocked(invoke);
  ipc.mockImplementation(async (command) => {
    if (command === "workflow_graph_registry") return [plan];
    if (command === "workflow_package_catalog") return [{ name: "Review with notes", version, blockers: [] }];
    if (command === "describe_workflow_reusable_node") return {
      artifact: { name: "Review with notes", version, blockers: [], body: { kind: "preset" } },
      definition,
      editableOverrides: ["role"],
    };
    throw new Error(command);
  });
  const user = userEvent.setup();
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <PipelinesWorkspace checkout={exampleCheckout} simulation={null} target="library" openWorkspace={vi.fn()}
      startSimulation={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);
  const card = await screen.findByRole("button", { name: "Review with notes" });
  expect(ipc).not.toHaveBeenCalledWith("describe_workflow_reusable_node", expect.anything());
  await user.click(card);
  const dialog = within(screen.getByRole("dialog", { name: "Review with notes" }));
  expect(await dialog.findByText("Agent role")).toBeTruthy();
  expect(dialog.queryByText("Stage model override")).toBeNull();
  expect(dialog.getByText(/Editable fields: role/)).toBeTruthy();
  expect(within(dialog.getByRole("region", { name: "Inputs" })).getByText("notes")).toBeTruthy();
  expect(ipc).toHaveBeenCalledWith("describe_workflow_reusable_node", { projectId: exampleCheckout.projectId, version });
});

it("explains an example preset without treating its simulation contract as a live schema", async () => {
  render(<PipelineCatalog library />);
  const user = userEvent.setup();
  await user.selectOptions(screen.getByRole("combobox", { name: "Category" }), "Presets");
  await user.click(screen.getByRole("button", { name: "Build" }));
  const dialog = within(screen.getByRole("dialog", { name: "Build" }));
  expect(dialog.getByText("Presets · Simulation")).toBeTruthy();
  expect(dialog.getByText("Compile a candidate")).toBeTruthy();
  expect(dialog.getByText("Registered build command")).toBeTruthy();
  expect(dialog.getByText(/no editable settings in the simulated Builder/)).toBeTruthy();
});

it("opens the graph from the exact saved workflow revision in Roles & templates", async () => {
  const version = { id: "release-template", revision: "pinned-workflow-revision", kind: "pipeline", destination: "project" };
  const name = "Release template";
  const body = { kind: "pipeline", graph: { name, nodes: [{ id: "start", kind: "start", configuration: { kind: "none" } }], wires: [] } };
  const ipc = vi.mocked(invoke);
  const openWorkspace = vi.fn();
  ipc.mockImplementation(async (command) => {
    if (command === "list_conversation_templates") return [];
    if (command === "workflow_package_catalog") return [{ name, version }];
    if (command === "read_published_workflow_package_artifact") return {
      name, version, definitions: { start: old }, references: {}, layout: { start: { x: 72, y: 15 } },
      body,
    };
    if (command === "create_workflow_package") return { id: "revision-draft" };
    throw new Error(command);
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <PipelinesWorkspace checkout={exampleCheckout} simulation={null} target="templates" openWorkspace={openWorkspace}
      startSimulation={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  const history = within(screen.getByRole("region", { name: "Saved workflow templates" }));
  await user.click(await history.findByRole("button", { name: /Release template/ }));
  expect(ipc).toHaveBeenCalledWith("read_published_workflow_package_artifact", { projectId: exampleCheckout.projectId, version });
  const template = within(await history.findByRole("region", { name: "Selected workflow template" }));
  const card = await template.findByTestId("node-start");
  expect(card.getAttribute("data-position")).toBe(JSON.stringify({ x: 72, y: 15 }));
  expect(card.querySelector('[data-port-handle="source:old-document"]')).toBeTruthy();
  expect(template.getByRole<HTMLButtonElement>("button", { name: "Add node" }).disabled).toBe(true);
  expect(template.getByRole("button", { name: "Run published workflow" })).toBeTruthy();
  await user.click(template.getByRole("button", { name: "Edit in Builder" }));
  expect(ipc).toHaveBeenCalledWith("create_workflow_package", {
    projectId: exampleCheckout.projectId, conversationId: null,
    package: { name, members: [{ key: "pipeline", name, destination: "project", base: version, body, references: {} }] },
  });
  await waitFor(() => expect(openWorkspace).toHaveBeenCalledWith("pipelines", "authoring:package:revision-draft"));
});

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
  const mount = (target?: string) => render(<QueryClientProvider client={client()}><PipelinesWorkspace {...props} target={target} /></QueryClientProvider>);
  const user = userEvent.setup();
  const list = mount();
  const builder = screen.getByRole("button", { name: "Builder" });
  expect(builder.getAttribute("aria-current")).toBe("page");
  await user.click(builder);
  expect(props.openWorkspace).toHaveBeenCalledWith("pipelines", "drafts");
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
  expect(catalog.queryByRole("button", { name: "Add Load context" })).toBeNull();
  await user.click(catalog.getByRole("button", { name: "Add Plan" }));
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
  const subflow: WorkflowDefinition = { ...iteration, kind: "subflow", configuration: "none", title: "Check result",
    inputs: [control("ready"), data("item", "json_item"), data("wrong", "markdown")],
    outputs: [control("completed"), data("result", "markdown"), data("check", "checks")] };
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
      problems={graphProblems(edited, definitions)} disabled={false} allowDefinitionUpdates={false}
      subflowDefinitionForNode={(id) => id === "for-each" ? subflow : undefined} />;
  }

  render(<Editor />);
  const user = userEvent.setup();
  const inspector = within(screen.getByRole("complementary", { name: "Node configuration" }));
  await user.selectOptions(inspector.getByRole("combobox", { name: "Inspect node" }), "list");
  await user.click(inspector.getByRole("button", { name: "Add item" }));
  await user.selectOptions(inspector.getByRole("combobox", { name: "Item 1 type" }), "number");
  await user.click(inspector.getByRole("button", { name: "Add item" }));
  await user.selectOptions(inspector.getByRole("combobox", { name: "Item 2 type" }), "boolean");
  expect(edited.nodes.find(({ id }) => id === "list")?.configuration).toEqual({ kind: "build_list", source: "values", values: [0, false] });
  await user.selectOptions(inspector.getByRole("combobox", { name: "List source" }), "evidence");
  fireEvent.change(inspector.getByRole("textbox", { name: /Array path/ }), { target: { value: "/evidence/items" } });
  expect(edited.nodes.find(({ id }) => id === "list")?.configuration).toEqual({ kind: "build_list", source: "evidence", pointer: "/evidence/items" });
  expect(screen.getByText(/list.value requires a connection/)).toBeTruthy();

  await user.click(screen.getByRole("button", { name: "Add node" }));
  const nodes = within(screen.getByRole("region", { name: "Node catalog" }));
  expect(nodes.queryByRole("button", { name: "Add Pin iteration list" })).toBeNull();
  await user.click(nodes.getByRole("button", { name: "Add For each" }));
  fireEvent.change(inspector.getByRole("spinbutton", { name: "Maximum items" }), { target: { value: "12" } });
  expect(within(inspector.getByRole("combobox", { name: "Subflow item input" })).queryByRole("option", { name: /wrong/ })).toBeNull();
  await user.selectOptions(inspector.getByRole("combobox", { name: "Subflow result output" }), "check");
  await user.selectOptions(inspector.getByRole("combobox", { name: "Source node" }), "list");
  await user.selectOptions(inspector.getByRole("combobox", { name: "Output port" }), "items");
  await user.selectOptions(inspector.getByRole("combobox", { name: "Input port" }), "items");
  await user.click(inspector.getByRole("button", { name: "Connect ports" }));
  expect(edited.nodes.find(({ id }) => id === "for-each")?.configuration).toEqual({
    kind: "for_each", maxItems: 12, readyInput: "ready", itemInput: "item", completionOutput: "completed", resultOutput: "check",
  });
  expect(edited.wires).toContainEqual(wire("list", "items", "for-each", "items"));
});

it("connects named ports with the keyboard, validates replacements, and restores the exact graph on undo", async () => {
  const included = [start, context];
  const existing = wire("start", "document", "context", "document");
  let edited: WorkflowGraph = { name: "Keyboard connections", nodes: [
    { id: "start", kind: "start", configuration: { kind: "none" } },
    { id: "context", kind: "load_context", configuration: { kind: "none" } },
    { id: "other", kind: "load_context", configuration: { kind: "none" } },
  ], wires: [existing] };
  function Editor() {
    const [state, setState] = useState<WorkflowEditorState>({ graph: edited, layout: {}, updateDefinitions: {} });
    edited = state.graph;
    return <WorkflowGraphEditor projectId="project" state={state} setState={setState} definitions={{ start, context, other: context }}
      catalog={included} problems={[]} disabled={false} />;
  }
  render(<Editor />);
  const user = userEvent.setup();
  async function press(name: string) {
    screen.getByRole("button", { name }).focus();
    await user.keyboard("{Enter}");
  }

  await press("Connect from start.start");
  await press("Connect to context.ready");
  const first = wire("start", "start", "context", "ready");
  expect(edited.wires).toEqual([existing, first]);

  await press("Reconnect start.document → context.document");
  await press("Connect to other.ready");
  expect(screen.getByRole("alert").textContent).toContain("cannot connect");
  expect(edited.wires).toEqual([existing, first]);
  await press("Connect to other.document");
  const moved = wire("start", "document", "other", "document");
  expect(edited.wires).toEqual([moved, first]);

  await press("Undo graph edit");
  expect(edited.wires).toEqual([existing, first]);
  await press("Redo graph edit");
  expect(edited.wires).toEqual([moved, first]);
});

it("edits typed launch fields and a branch condition without raw configuration", async () => {
  const entry: WorkflowDefinition = { ...start, kind: "typed_start", configuration: "typed_start" };
  const condition: WorkflowDefinition = { ...context, kind: "condition", title: "Condition", configuration: "condition" };
  let edited: WorkflowGraph = { name: "Typed values", nodes: [
    { id: "start", kind: "typed_start", configuration: defaultConfiguration("typed_start") },
    { id: "condition", kind: "condition", configuration: defaultConfiguration("condition") },
  ], wires: [] };
  function Editor() {
    const [state, setState] = useState<WorkflowEditorState>({ graph: edited, layout: {}, updateDefinitions: {} });
    edited = state.graph;
    return <WorkflowGraphEditor projectId="project" state={state} setState={setState} definitions={{ start: entry, condition }}
      catalog={[start, entry, condition]} problems={[]} disabled={false} />;
  }
  render(<Editor />);
  const user = userEvent.setup();
  const inspector = within(screen.getByRole("complementary", { name: "Node configuration" }));
  await user.click(inspector.getByRole("button", { name: "Add input" }));
  fireEvent.change(inspector.getByRole("textbox", { name: "Input 1 name" }), { target: { value: "approved" } });
  await user.selectOptions(inspector.getByRole("combobox", { name: "Input 1 type" }), "boolean");
  expect(edited.nodes[0]?.configuration).toEqual({ kind: "typed_start", fields: [{ name: "approved", valueType: "boolean", required: true }] });
  await user.selectOptions(inspector.getByRole("combobox", { name: "Input 1 source" }), "history");
  expect(inspector.getByRole<HTMLSelectElement>("combobox", { name: "Input 1 type" }).disabled).toBe(true);
  expect(edited.nodes[0]?.configuration).toEqual({ kind: "typed_start", fields: [{
    name: "approved", valueType: "array", required: true, conversationSource: "history",
  }] });
  await user.selectOptions(inspector.getByRole("combobox", { name: "Input 1 source" }), "message");
  expect(edited.nodes[0]?.configuration).toEqual({ kind: "typed_start", fields: [{
    name: "approved", valueType: "text", required: true, conversationSource: "message",
  }] });
  await user.selectOptions(inspector.getByRole("combobox", { name: "Input 1 source" }), "");
  expect(inspector.getByRole<HTMLSelectElement>("combobox", { name: "Input 1 type" }).disabled).toBe(false);
  expect(edited.nodes[0]?.configuration).toEqual({ kind: "typed_start", fields: [{ name: "approved", valueType: "text", required: true }] });
  expect(configurationProblem({ kind: "typed_start", fields: [{
    name: "approved", valueType: "text", required: true, conversationSource: "history",
  }] }, entry)).toMatch(/Array for conversation history/);

  await user.selectOptions(inspector.getByRole("combobox", { name: "Inspect node" }), "condition");
  await user.selectOptions(inspector.getByRole("combobox", { name: "Test" }), "equals");
  fireEvent.change(inspector.getByRole("textbox", { name: "Field path" }), { target: { value: "/approved" } });
  await user.selectOptions(inspector.getByRole("combobox", { name: "Expected value type" }), "boolean");
  expect(edited.nodes[1]?.configuration).toEqual({ kind: "condition", predicate: { op: "equals", pointer: "/approved", value: false } });
  expect(inspector.queryByRole("textbox", { name: "Node parameters" })).toBeNull();
});

it("matches the backend rules for Start events and optional or multiple Result evidence", () => {
  const definitions: Record<string, WorkflowDefinition> = {
    entry: { ...start, kind: "typed_start", configuration: "typed_start", outputs: [...start.outputs, data("event", "agile_event")] },
    text: { kind: "build_text", title: "Build text", revision: 1, configuration: "build_text", executor: "build_text",
      effect: "read_evidence", inputs: [control("ready")], outputs: [control("completed"), data("text", "rendered_text")] },
    result: { kind: "summarize_result", title: "Result", revision: 1, configuration: "none", executor: "summarize_result",
      effect: "read_evidence", inputs: [control("ready"), data("summary", "rendered_text"), { name: "evidence", value: { kind: "evidence" } }], outputs: [] },
  };
  const event = wire("entry", "event", "result", "evidence");
  const otherEvidence = wire("entry", "document", "result", "evidence");
  const graph: WorkflowGraph = { name: "Work results", nodes: [
    { id: "entry", kind: "typed_start", configuration: { kind: "typed_start", fields: [{ name: "owner", valueType: "text", required: true }] } },
    { id: "text", kind: "build_text", configuration: { kind: "build_text", template: "Result text" } },
    { id: "result", kind: "summarize_result", configuration: { kind: "none" } },
  ], wires: [wire("entry", "start", "text", "ready"), wire("text", "completed", "result", "ready"), wire("text", "text", "result", "summary")] };
  expect(graphProblems(graph, definitions)).toEqual([]);
  const diagnostic = "Make Start fields optional to connect the event port.";
  expect(connectionProblem(graph, definitions, event)).toBe(diagnostic);
  graph.wires.push(event);
  expect(graphProblems(graph, definitions)).toEqual([diagnostic]);
  graph.nodes[0]!.configuration = defaultConfiguration("typed_start");
  expect(graphProblems(graph, definitions)).toEqual([]);
  expect(connectionProblem(graph, definitions, otherEvidence)).toBeNull();
});
