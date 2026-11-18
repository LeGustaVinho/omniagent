// @vitest-environment jsdom

import { useEffect, useState, type ComponentType, type MouseEvent } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import PipelinesWorkspace from "@/pipelines-workspace";
import { PipelineAuthoringEditor } from "@/workflow-authoring";
import type { PackageDraft } from "@/workflow-authoring-package";
import { editorDefinitions, WorkflowGraphEditor, type WorkflowEditorState } from "@/workflow-graph-editor";
import { configurationProblem, connectionProblem, defaultConfiguration, graphProblems, wireKey, type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowGraph, type WorkflowPort } from "@/workflow-graph-model";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const { fitView } = vi.hoisted(() => ({ fitView: vi.fn() }));

vi.mock("@xyflow/react", async (importOriginal) => {
  type Position = { x: number; y: number };
  type Card = { id: string; position: Position; selected?: boolean; data: { definition: WorkflowDefinition } };
  type Wire = { id: string };
  const update = () => undefined;
  const flow = { screenToFlowPosition: (point: Position) => point, fitView };
  return {
    ...await importOriginal<typeof import("@xyflow/react")>(),
    Background: () => null,
    Controls: () => null,
    Handle: ({ id, type }: { id: string; type: string }) => <span data-port-handle={`${type}:${id}`} />,
    useUpdateNodeInternals: () => update,
    ReactFlow: ({ nodes, edges, nodeTypes, onNodesChange, onNodeContextMenu, onEdgeContextMenu, onPaneContextMenu, onSelectionContextMenu, onInit }: {
      nodes: Card[]; nodeTypes: { workflow: ComponentType<Card> };
      edges: Wire[];
      onNodesChange: (changes: ({ type: "position"; id: string; position: Position } | { type: "select"; id: string; selected: boolean })[]) => void;
      onNodeContextMenu?: (event: MouseEvent, node: Card) => void;
      onEdgeContextMenu?: (event: MouseEvent, wire: Wire) => void;
      onPaneContextMenu?: (event: MouseEvent) => void;
      onSelectionContextMenu?: (event: MouseEvent, nodes: Card[]) => void;
      onInit?: (instance: typeof flow) => void;
    }) => {
      useEffect(() => { onInit?.(flow); }, [onInit]);
      const selected = nodes.filter((node) => node.selected);
      return <div data-testid="test-pipeline-pane" onContextMenu={onPaneContextMenu}>
      {selected.length > 1 && <div data-testid="pipeline-selection" className="react-flow__nodesselection-rect"
        onContextMenu={(event) => onSelectionContextMenu?.(event, selected)} />}
      {nodes.map(({ id, ...card }) => <div key={id} className="react-flow__node" data-id={id} data-testid={`node-${id}`}
        data-position={JSON.stringify(card.position)} onContextMenu={(event) => onNodeContextMenu?.(event, { id, ...card })}>
        <nodeTypes.workflow id={id} {...card} />
        <button type="button" onClick={() => onNodesChange([{ type: "position", id, position: { x: 91, y: -8 } }])}>Move {id}</button>
        <button type="button" onClick={() => onNodesChange([{ type: "select", id, selected: true }])}>Select {id}</button>
      </div>)}
      {edges?.map((edge) => <div key={edge.id} data-testid={`edge-${edge.id}`} onContextMenu={(event) => onEdgeContextMenu?.(event, edge)} />)}
      </div>;
    },
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

it("opens actions for the clicked node, connection, and canvas independently", async () => {
  const connection = wire("start", "start", "context", "ready");
  const definitions = { start, context };
  function TestBuilder() {
    const [state, setState] = useState<WorkflowEditorState>({ graph: {
      name: "Test graph", wires: [connection], nodes: [
        { id: "start", kind: "start", configuration: { kind: "none" } },
        { id: "context", kind: "load_context", configuration: { kind: "none" } },
      ],
    }, layout: {}, updateDefinitions: {} });
    return <WorkflowGraphEditor projectId="test" state={state} setState={setState} definitions={definitions}
      catalog={[start, context]} problems={[]} disabled={false} />;
  }
  render(<TestBuilder />);
  const user = userEvent.setup();
  fireEvent.contextMenu(screen.getByTestId("node-context"));
  await user.click(within(await screen.findByRole("menu", { name: "context" })).getByRole("menuitem", { name: "Inspect node" }));
  expect((screen.getByRole("combobox", { name: "Inspect node" }) as HTMLSelectElement).value).toBe("context");
  expect(screen.getByTestId("node-context").querySelector(".workflow-node-card[data-selected]")).toBeTruthy();

  const edge = `edge-${wireKey(connection)}`;
  fireEvent.contextMenu(screen.getByTestId(edge));
  await user.click(await screen.findByRole("menuitem", { name: "Remove connection" }));
  expect(screen.queryByTestId(edge)).toBeNull();
  expect(screen.getByTestId("node-context")).toBeTruthy();

  fireEvent.contextMenu(screen.getByTestId("test-pipeline-pane"));
  await user.click(within(await screen.findByRole("menu", { name: "Pipeline canvas" })).getByRole("menuitem", { name: "Undo graph edit" }));
  expect(screen.getByTestId(edge)).toBeTruthy();
});

it("requires the reusable review's prompt and subject from the same evidence collector", () => {
  const collector: WorkflowDefinition = {
    kind: "collect_evidence", title: "Collect evidence", revision: 1, configuration: "none",
    executor: "collect_evidence", effect: "none", inputs: [control("ready")],
    outputs: [control("completed"), data("text", "rendered_text"), data("evidence", "evidence_set")],
  };
  const agent: WorkflowDefinition = {
    kind: "agent", title: "Agent", revision: 2, configuration: "agent_role", executor: "read_only_agent",
    effect: "invoke_model", inputs: [control("ready"), data("prompt", "rendered_text"),
      data("comparison", "comparison_results"), data("review_subject", "evidence_set")], outputs: [control("completed")],
  };
  const definitions = { start, brief: collector, other: collector, agent };
  const graph: WorkflowGraph = {
    name: "Selective review",
    nodes: [
      { id: "start", kind: "start", configuration: { kind: "none" } },
      { id: "brief", kind: "collect_evidence", configuration: { kind: "none" } },
      { id: "other", kind: "collect_evidence", configuration: { kind: "none" } },
      { id: "agent", kind: "agent", configuration: { kind: "agent_role", role: "reviewer", requirements: { reviewReuse: true } } },
    ],
    wires: [
      wire("start", "start", "brief", "ready"), wire("start", "start", "other", "ready"),
      wire("brief", "completed", "agent", "ready"), wire("brief", "text", "agent", "prompt"),
    ],
  };
  const diagnostic = "agent review reuse requires prompt and review subject from the same Collect Evidence node.";
  expect(graphProblems(graph, definitions)).toEqual([diagnostic]);
  expect(graphProblems({ ...graph, wires: [...graph.wires, wire("other", "evidence", "agent", "review_subject")] },
    definitions)).toEqual([diagnostic]);
  expect(graphProblems({ ...graph, wires: [...graph.wires, wire("brief", "evidence", "agent", "review_subject")] },
    definitions)).toEqual([]);
});

it("loads published Library artifacts without loading the Builder node registry", async () => {
  const ipc = vi.mocked(invoke);
  ipc.mockClear();
  ipc.mockResolvedValue([]);
  render(<QueryClientProvider client={new QueryClient()}>
    <PipelinesWorkspace checkout={exampleCheckout} simulation={null} target="library" openWorkspace={vi.fn()}
      startSimulation={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);
  const library = within(screen.getByRole("region", { name: "Workflow library" }));
  expect(await library.findByText("No published schemas.")).toBeTruthy();
  expect(library.queryByRole("searchbox")).toBeNull();
  expect(library.queryByText("Import or export workflow packages")).toBeNull();
  expect(ipc).not.toHaveBeenCalledWith("workflow_node_definitions", expect.anything());
  expect(ipc).toHaveBeenCalledWith("workflow_package_catalog", { projectId: exampleCheckout.projectId });
});

it.each(["Node Prefab", "Function"] as const)("creates a %s from Library and opens its editor", async (title) => {
  const schema = { kind: "schema", destination: "project", id: "schema", revision: "schema-revision" };
  const useFunction = title === "Function";
  const end: WorkflowDefinition = { ...start, kind: "end", title: "End", configuration: "end",
    inputs: [control("ready"), data("result", "json_object")], outputs: [control("completed"), data("result", "json_object")] };
  const ipc = vi.mocked(invoke);
  let draft: PackageDraft | undefined;
  const projectId = exampleCheckout.projectId;
  ipc.mockImplementation(async (command, payload) => {
    switch (command) {
      case "list_workflow_packages": return draft ? [{ id: draft.id, name: draft.package.name, memberCount: 1, standalonePipeline: false, published: false }] : [];
      case "workflow_package_catalog": return [{ name: "Request object", version: schema, blockers: [] }];
      case "create_workflow_package":
        draft = { id: "new-artifact", revision: "initial", conversationId: null, publication: null, request: null,
          package: (payload as { package: PackageDraft["package"] }).package };
        return draft;
      case "inspect_workflow_package": return draft;
      case "preview_workflow_package_revision": return { revision: draft?.revision, revisions: [draft?.revision], members: [] };
      case "workflow_node_definitions": return [start, end];
      case "read_published_workflow_package_artifact": return { body: { kind: "schema", fields: [{ name: "message", type: "string", required: true }] } };
      case "save_workflow_package":
        draft = { ...draft!, revision: "configured", package: (payload as { package: PackageDraft["package"] }).package };
        return draft;
      default: throw new Error(command);
    }
  });
  function Workspace() {
    const [target, setTarget] = useState("library");
    return <PipelinesWorkspace checkout={exampleCheckout} simulation={null} target={target}
      openWorkspace={(_, next) => setTarget(next ?? "")} startSimulation={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} />;
  }
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <Workspace />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: `New ${title}` }));
  const dialog = within(screen.getByRole("dialog", { name: `New ${title}` }));
  const create = dialog.getByRole<HTMLButtonElement>("button", { name: `Create ${title}` });
  expect(create.disabled).toBe(true);
  await user.type(dialog.getByRole("textbox", { name: "Name" }), "  Analyze requests  ");
  await user.click(create);
  expect(ipc).toHaveBeenCalledWith("create_workflow_package", {
    projectId, conversationId: null, package: expect.objectContaining({ name: "Analyze requests", members: [expect.objectContaining({
      name: "Analyze requests", body: expect.objectContaining(useFunction ? { kind: "subflow",
        inputs: { ready: { node: "start", port: "ready" }, input: { node: "start", port: "inputs" } },
        outputs: { completed: { node: "return", port: "completed" }, result: { node: "return", port: "result" } },
      } : { kind: "prompt", prompt: expect.objectContaining({ schemaContract: true, nodePrefab: true }) }),
    })] }),
  });

  if (useFunction) {
    expect(await screen.findByRole<HTMLInputElement>("textbox", { name: "Function name" })).toHaveProperty("value", "Analyze requests");
    const chooseInput = within(screen.getByRole("combobox", { name: "Input Schema" }));
    await user.selectOptions(screen.getByRole("combobox", { name: "Input Schema" }), chooseInput.getByRole("option", { name: /Request object/ }));
    await user.selectOptions(screen.getByRole("combobox", { name: "Inspect node" }), "return");
    expect(screen.getByRole("heading", { name: "Return" })).toBeTruthy();
    const chooseOutput = screen.getByRole("combobox", { name: "Output Schema" });
    await user.selectOptions(chooseOutput, within(chooseOutput).getByRole("option", { name: /Request object/ }));
    await user.click(screen.getByRole("button", { name: "Save draft revision" }));
    expect(ipc).toHaveBeenCalledWith("save_workflow_package", expect.objectContaining({ package: expect.objectContaining({
      members: [expect.objectContaining({ references: {
        "start:schema": { source: "published", version: schema }, "return:schema": { source: "published", version: schema },
      } })],
    }) }));
  } else {
    const settings = within(await screen.findByLabelText("AI Node Agent settings"));
    expect(settings.getByRole("textbox", { name: "Card title" })).toHaveProperty("value", "Analyze requests");
    expect(settings.getByRole("textbox", { name: "Prompt" })).toBeTruthy();
  }
  await user.click(screen.getByRole("button", { name: "Library" }));
  expect(await screen.findByRole("button", { name: "Open draft of Analyze requests" })).toBeTruthy();
});

it("uses the canvas node picker and saves an AI Node with its position and Schema references", async () => {
  const typedStart = { ...start, kind: "typed_start", configuration: "typed_start" };
  const end = { ...start, kind: "end", title: "End", configuration: "end", inputs: [control("ready")], outputs: [control("completed")] };
  const subflow = { ...start, kind: "subflow", title: "Function", inputs: [], outputs: [] };
  const role = { destination: "project", kind: "role", id: "summarizer", revision: "role-one" };
  const spec = (key: string, name: string) => ({ key, name, body: { kind: "schema", fields: [] } });
  const current = {
    id: "draft", revision: "saved", conversationId: null, publication: null, request: null,
    package: { name: "Summarize", members: [
      { key: "workflow", name: "Summarize", body: { kind: "pipeline", graph: {
        name: "Summarize",
        nodes: [{ id: "start", kind: "typed_start", configuration: defaultConfiguration("typed_start") },
          { id: "end", kind: "end", configuration: defaultConfiguration("end") }],
        wires: [wire("start", "start", "end", "ready")],
      } } }, spec("incoming", "Incoming"), spec("outgoing", "Outgoing"),
    ] },
  };
  const ipc = vi.mocked(invoke);
  ipc.mockImplementation(async (command, payload) => {
    switch (command) {
      case "inspect_workflow_package": return current;
      case "preview_workflow_package_revision": return { revision: current.revision, revisions: [current.revision], members: [] };
      case "workflow_node_definitions": return [typedStart, end, subflow];
      case "workflow_package_catalog": return [{ version: role, name: "Summarizer", blockers: [] }];
      case "read_published_workflow_package_artifact": return { body: { kind: "role", template: {
        name: "Summarizer", role: "agent", model: { provider: "codex", profileId: "default", modelId: "model" }, effort: null,
        instructions: "", completionContract: "", toolScope: "read_only", reviewPolicy: "independent",
      } } };
      case "save_workflow_package": return { ...current, revision: "configured", package: (payload as { package: unknown }).package };
      default: throw new Error(command);
    }
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <PipelineAuthoringEditor checkout={exampleCheckout} draftId="package:draft" simulation={null} openWorkspace={vi.fn()}
      startSimulation={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await screen.findByRole("button", { name: "Add node" });
  const canvas = screen.getByLabelText("Workflow canvas actions");
  canvas.focus();
  fireEvent.keyDown(canvas, { key: "F10", shiftKey: true });
  expect(await screen.findByRole("dialog", { name: "Add node" })).toBeTruthy();
  await user.keyboard("{Escape}");
  await waitFor(() => expect(document.activeElement).toBe(canvas));

  fireEvent.contextMenu(screen.getByTestId("test-pipeline-pane"), { clientX: 325, clientY: 185 });
  await user.click(within(await screen.findByRole("dialog", { name: "Add node" })).getByRole("button", { name: "AI Node Agent" }));
  expect(screen.getByTestId("node-subflow").getAttribute("data-position")).toBe(JSON.stringify({ x: 325, y: 185 }));
  const settings = within(await screen.findByLabelText("AI Node Agent settings"));
  await user.selectOptions(settings.getByRole("combobox", { name: "Role" }), settings.getByRole("option", { name: /^Summarizer/ }));
  const input = settings.getByRole("combobox", { name: "Input Schema" });
  const output = settings.getByRole("combobox", { name: "Output Schema" });
  await user.selectOptions(input, within(input).getByRole("option", { name: /^Incoming/ }));
  await user.selectOptions(output, within(output).getByRole("option", { name: /^Outgoing/ }));
  await user.type(settings.getByRole("textbox", { name: "Prompt" }), "Summarize the message.");
  await user.click(screen.getByRole("button", { name: "Save draft revision" }));
  expect(ipc).toHaveBeenCalledWith("save_workflow_package", expect.objectContaining({ canvasByMember: {
    workflow: { layout: { subflow: { x: 325, y: 185 } }, updateDefinitions: {} },
  }, package: expect.objectContaining({
    members: expect.arrayContaining([expect.objectContaining({
      key: "agent",
      body: { kind: "prompt", prompt: expect.objectContaining({ text: "Summarize the message.", schemaContract: true }) },
      references: { role: { source: "published", version: role }, input_schema: { source: "draft", key: "incoming" },
        output_schema: { source: "draft", key: "outgoing" } },
    })]),
  }) }));
});

it("duplicates the selected graph with its internal wires, exact old pin and undo history", async () => {
  const internal = wire("context", "loaded", "plan", "ready");
  const original: WorkflowEditorState = { graph: { name: "Pinned stages", nodes: [
    { id: "start", kind: "start", configuration: { kind: "none" } },
    { id: "context", kind: "load_context", configuration: { kind: "none" } },
    { id: "plan", kind: "plan", configuration: { kind: "agent_role", role: "Planner" } },
  ], wires: [wire("start", "start", "context", "ready"), internal] },
  layout: { context: { x: 10, y: 20 }, plan: { x: 180, y: 90 } }, updateDefinitions: {} };
  const catalog = [start, { ...context, revision: 2 }, plan];
  let observed = original;
  function Editor() {
    const [state, setState] = useState(original);
    observed = state;
    return <WorkflowGraphEditor projectId="project" state={state} setState={setState}
      definitions={editorDefinitions(state, { start, context, plan }, catalog)} catalog={catalog} problems={[]} disabled={false} />;
  }
  render(<QueryClientProvider client={new QueryClient()}><Editor /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Select context" }));
  await user.click(screen.getByRole("button", { name: "Select plan" }));
  fireEvent.contextMenu(screen.getByTestId("node-context"));
  await user.click(await screen.findByRole("menuitem", { name: "Copy 2 selected nodes" }));
  fireEvent.contextMenu(screen.getByTestId("node-context"));
  await user.click(await screen.findByRole("menuitem", { name: "Duplicate 2 selected nodes" }));
  expect(observed.definitionCopies).toEqual({ "context-2": { sourceId: "context", revision: 1 } });
  expect(observed.layout["context-2"]).toEqual({ x: 46, y: 56 });
  expect(observed.graph.nodes.slice(-2)).toEqual([
    { ...original.graph.nodes[1], id: "context-2" }, { ...original.graph.nodes[2], id: "plan-2" },
  ]);
  expect(observed.graph.wires).toEqual([...original.graph.wires, wire("context-2", "loaded", "plan-2", "ready")]);
  expect(screen.getByText("Version 1")).toBeTruthy();
  expect(screen.getByText("read evidence")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Undo graph edit" }));
  expect(observed).toEqual(original);
  await user.click(screen.getByRole("button", { name: "Redo graph edit" }));
  expect(observed.graph.wires).toHaveLength(3);
  fireEvent.contextMenu(screen.getByTestId("test-pipeline-pane"), { clientX: 430, clientY: 240 });
  await user.click(await screen.findByRole("menuitem", { name: "Paste 2 nodes here" }));
  expect(observed.layout["context-3"]).toEqual({ x: 430, y: 240 });
  expect(observed.layout["plan-3"]).toEqual({ x: 600, y: 310 });
  expect(observed.definitionCopies?.["context-3"]).toEqual({ sourceId: "context", revision: 1 });
  expect(observed.graph.wires[3]).toEqual(wire("context-3", "loaded", "plan-3", "ready"));
});

it("fits and removes the exact node selection in one undoable operation", async () => {
  const original: WorkflowEditorState = { graph: { name: "Exact selection", nodes: [
    { id: "start", kind: "start", configuration: { kind: "none" } },
    { id: "context", kind: "load_context", configuration: { kind: "none" } },
    { id: "plan", kind: "plan", configuration: { kind: "agent_role", role: "Planner" } },
    { id: "other", kind: "load_context", configuration: { kind: "none" } },
  ], wires: [
    wire("start", "start", "context", "ready"), wire("context", "loaded", "plan", "ready"),
    wire("start", "start", "other", "ready"),
  ] },
  layout: { context: { x: 10, y: 20 }, plan: { x: 180, y: 90 } }, updateDefinitions: { context: 2 },
  definitionCopies: { plan: { sourceId: "published-plan", revision: 1 } },
  nodeBindings: { context: { reference: { source: "draft", key: "local-context" } } } };
  let observed = original;
  function Editor() {
    const [state, setState] = useState(original);
    observed = state;
    return <WorkflowGraphEditor projectId="project" state={state} setState={setState} definitions={{ start, context, plan, other: context }}
      catalog={[start, context, plan]} problems={[]} disabled={false} />;
  }
  render(<QueryClientProvider client={new QueryClient()}><Editor /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Select context" }));
  await user.click(screen.getByRole("button", { name: "Select plan" }));

  fireEvent.contextMenu(screen.getByTestId("node-other"));
  await user.click(await screen.findByRole("menuitem", { name: "Remove node" }));
  expect(observed.graph.nodes.map(({ id }) => id)).toEqual(["start", "context", "plan"]);
  await user.click(screen.getByRole("button", { name: "Undo graph edit" }));
  expect(observed).toEqual(original);

  fireEvent.contextMenu(screen.getByTestId("node-context"));
  expect(await screen.findByRole("menuitem", { name: "Remove 2 selected nodes" })).toBeTruthy();
  expect(screen.queryByRole("menuitem", { name: "Remove node" })).toBeNull();
  await user.keyboard("{Escape}");

  fireEvent.contextMenu(screen.getByTestId("pipeline-selection"));
  const group = within(await screen.findByRole("menu", { name: "2 selected nodes" }));
  await user.click(group.getByRole("menuitem", { name: "Fit selection to view" }));
  expect(fitView).toHaveBeenCalledWith({ nodes: [{ id: "context" }, { id: "plan" }], padding: 0.15, maxZoom: 1 });
  expect(observed).toEqual(original);
  fireEvent.keyDown(screen.getByTestId("pipeline-selection"), { key: "F10", shiftKey: true });
  await user.click(await screen.findByRole("menuitem", { name: "Remove 2 selected nodes" }));
  expect(observed.graph.nodes.map(({ id }) => id)).toEqual(["start", "other"]);
  expect(observed.graph.wires).toEqual([wire("start", "start", "other", "ready")]);
  expect(observed.layout).toEqual({});
  expect(observed.updateDefinitions).toEqual({});
  expect(observed.definitionCopies).toBeUndefined();
  expect(observed.nodeBindings).toEqual({});
  await user.click(screen.getByRole("button", { name: "Undo graph edit" }));
  expect(observed).toEqual(original);
});

it("reconnects only the edge source after validating the new output", async () => {
  const oldEdge = wire("start", "document", "context", "document");
  const newEdge = wire("other", "context", "context", "document");
  const original: WorkflowEditorState = { graph: { name: "Reconnect source", nodes: [
    { id: "start", kind: "start", configuration: { kind: "none" } },
    { id: "context", kind: "load_context", configuration: { kind: "none" } },
    { id: "other", kind: "load_context", configuration: { kind: "none" } },
  ], wires: [oldEdge] }, layout: {}, updateDefinitions: {} };
  let observed = original;
  function Editor() {
    const [state, setState] = useState(original);
    observed = state;
    return <WorkflowGraphEditor projectId="project" state={state} setState={setState} definitions={{ start, context, other: context }}
      catalog={[start, context]} problems={[]} disabled={false} />;
  }
  render(<Editor />);
  const user = userEvent.setup();
  fireEvent.contextMenu(screen.getByTestId(`edge-${wireKey(oldEdge)}`));
  await user.click(await screen.findByRole("menuitem", { name: "Reconnect source…" }));
  expect(screen.getByRole("status").textContent).toBe("Select an output port for context.document.");
  await user.click(screen.getByRole("button", { name: "Connect from other.loaded" }));
  expect(screen.getByRole("alert").textContent).toContain("cannot connect to context.document");
  expect(observed).toEqual(original);
  await user.click(screen.getByRole("button", { name: "Connect from other.context" }));
  expect(observed.graph.wires).toEqual([newEdge]);
  expect(screen.queryByRole("status")).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Undo graph edit" }));
  expect(observed).toEqual(original);
});

it("opens a protected draft, reviews an affected port upgrade, wires typed nodes, and restores the saved graph and layout", async () => {
  const graph: WorkflowGraph = {
    name: "Versioned review",
    nodes: [{ id: "start", kind: "start", configuration: { kind: "none" } }, { id: "context", kind: "load_context", configuration: { kind: "none" } },
      { id: "plan", kind: "plan", configuration: { kind: "agent_role", role: "" } }],
    wires: [wire("start", "start", "context", "ready"), wire("start", "old-document", "context", "document")],
  };
  const original = {
    id: "draft-1", conversationId: "chat-1", base: null,
    head: { id: "first-revision", runId: null, graph, layout: {}, definitions: { start: old, context, plan } },
    request: { jobId: "job-1", runId: null, state: "saved", diagnostic: null },
  };
  let stored = original;
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    switch (command) {
      case "project_workflow_graph_drafts":
        expect(input).toEqual({ projectId: exampleCheckout.projectId });
        return [{ id: stored.id, name: stored.head.graph.name, revision: stored.head.id }];
      case "inspect_workflow_authoring": return stored;
      case "workflow_node_definitions": return [start, context, plan];
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
  expect(screen.queryByRole("button", { name: "Repository delivery" })).toBeNull();
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
  expect(screen.queryByRole("button", { name: "Add node" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Add a reusable node" })).toBeNull();
  expect(screen.queryByRole("region", { name: "Node catalog" })).toBeNull();
  await user.selectOptions(inspector.getByRole("combobox", { name: "Inspect node" }), "plan");
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
    graph: { ...graph, nodes: graph.nodes.map((node) => node.id === "plan" ? { ...node, configuration: { kind: "agent_role", role: "Qualified planner" } } : node),
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
    { id: "for-each", kind: "for_each", configuration: defaultConfiguration("for_each") },
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

  await user.selectOptions(inspector.getByRole("combobox", { name: "Inspect node" }), "for-each");
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

it("exposes a typed port when a pinned Repeat until output is selected", async () => {
  const repeat: WorkflowDefinition = { kind: "repeat_until", title: "Repeat until", revision: 1,
    configuration: "repeat_until", executor: "repeat_subflow", effect: "none", inputs: [], outputs: [] };
  const body = { ...context, inputs: [control("ready"), data("document", "markdown")],
    outputs: [control("completed"), data("result", "markdown"), data("failed", "checks")] };
  let edited: WorkflowGraph = { name: "Approved output", nodes: [
    { id: "start", kind: "start", configuration: { kind: "none" } },
    { id: "repeat", kind: "repeat_until", configuration: defaultConfiguration("repeat_until") },
    { id: "context", kind: "load_context", configuration: { kind: "none" } },
  ], wires: [] };
  const catalog = [start, repeat, context];
  function Editor() {
    const [state, setState] = useState<WorkflowEditorState>({ graph: edited, layout: {}, updateDefinitions: {} });
    edited = state.graph;
    const subflow = (id: string) => id === "repeat" ? body : undefined;
    return <WorkflowGraphEditor projectId="project" state={state} setState={setState}
      definitions={editorDefinitions(state, { start, repeat, context }, catalog, subflow)} catalog={catalog}
      problems={[]} disabled={false} subflowDefinitionForNode={subflow} />;
  }
  render(<Editor />);
  const user = userEvent.setup();
  const inspector = within(screen.getByRole("complementary", { name: "Node configuration" }));
  await user.selectOptions(inspector.getByRole("combobox", { name: "Inspect node" }), "repeat");
  const exports = within(inspector.getByRole("group", { name: "Export from the approved iteration" }));
  expect(exports.queryByRole("checkbox", { name: "failed" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Connect from repeat.result" })).toBeNull();
  await user.click(exports.getByRole("checkbox", { name: "result" }));
  expect(edited.nodes.find(({ id }) => id === "repeat")?.configuration.exports).toEqual(["result"]);
  await user.click(screen.getByRole("button", { name: "Connect from repeat.result" }));
  await user.click(screen.getByRole("button", { name: "Connect to context.document" }));
  expect(edited.wires).toContainEqual(wire("repeat", "result", "context", "document"));
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

it("requires a conversation message in the selected Start Schema", () => {
  const graph: WorkflowGraph = { name: "Conversation template", nodes: [
    { id: "start", kind: "typed_start", configuration: { kind: "typed_start", conversation: true, fields: [] } },
    { id: "end", kind: "end", configuration: { kind: "end" } },
  ], wires: [wire("start", "start", "end", "ready")] };
  const definitions: Record<string, WorkflowDefinition> = {
    start: { ...start, kind: "typed_start", configuration: "typed_start",
      outputSchemas: { inputs: { type: "object", properties: { topic: { type: "string" } } } } },
    end: { ...start, kind: "end", configuration: "end", inputs: [control("ready")], outputs: [],
      outputSchemas: { result: { type: "object", properties: { reply: { type: "string" } }, required: ["reply"] } } },
  };
  expect(graphProblems(graph, definitions)).toEqual(["A conversation pipeline needs a text message input on Start."]);
  definitions.start!.outputSchemas = { inputs: { type: "object", properties: { message: { type: "string" } } } };
  expect(graphProblems(graph, definitions)).toEqual([]);
  const properties = definitions.start!.outputSchemas.inputs!.properties!;
  properties.history = { type: "array", items: { type: "string" } };
  expect(graphProblems(graph, definitions)).toEqual(["Start history must accept a list of objects with string fields role and text."]);
  properties.history.items = { type: "object", properties: { role: { type: "string" }, text: { type: "string" } },
    required: ["role", "text"], additionalProperties: false };
  expect(graphProblems(graph, definitions)).toEqual([]);
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
