// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { downloadArtifact } from "@/download-artifact";
import { WorkflowNodeEvidence } from "@/workflow-execution-evidence";
import type { ExecutionOutput, ExecutionPinnedNode, WorkflowExecutionSnapshot } from "@/workflow-execution-model";
import { testCheckout } from "@/test-fixtures/checkout";

vi.mock("@/download-artifact", async (original) => ({
  ...await original<typeof import("@/download-artifact")>(),
  downloadArtifact: vi.fn(),
}));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

function node(id: string): ExecutionPinnedNode {
  return {
    instance: { id, kind: "set_fields", configuration: { kind: "set_fields", operations: [] } },
    revision: `${id}-pinned`,
    definition: { kind: "set_fields", title: id, revision: 1, configuration: "set_fields",
      executor: "fixture", effect: "none", inputs: [], outputs: [] },
  };
}

it("copies and downloads the clicked historical output and follows only its recorded input references", async () => {
  const scope = { projectId: testCheckout.projectId, workflowRunId: "workflow-run", workflowVersion: { id: "workflow", revision: "pinned-workflow" } };
  const producer = node("producer");
  const consumer = node("consumer");
  function output(stage: ExecutionPinnedNode, id: string, evidence: unknown, current: boolean): ExecutionOutput {
    return { artifact: { id, workflowRunId: scope.workflowRunId,
      nodeInstanceId: stage.instance.id, port: "result", unitId: "recorded-unit", value: { kind: "json_object", evidence } },
    current, inputReceipts: [] };
  }
  const exactInput = output(producer, "older-source", { prompt: "Original input\r\n  prior revision" }, false);
  const currentInput = output(producer, "current-source", { prompt: "This input was not used" }, true);
  const exactOutput = output(consumer, "older-output", {
    text: `  ${"ação ".repeat(8000)}\r\nLast line  `,
    data: [{ selected: true, number: 17, value: null }],
  }, false);
  exactOutput.inputReceipts = [{ executionKey: "earlier-plan", revision: "exact-input-receipt",
    inputs: { document: exactInput.artifact.id, unavailable: "unknown-artifact" } }];
  const currentOutput = output(consumer, "current-output", { text: "Later output" }, true);
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: scope.workflowRunId, projectId: scope.projectId, version: scope.workflowVersion, planRevision: "new-plan" },
    definition: { version: scope.workflowVersion, name: "Recorded workflow", nodes: [producer, consumer], wires: [] },
    unitNodeIds: ["producer", "consumer"], units: [], jobs: [], control: [],
    outputs: [exactInput, currentInput, exactOutput, currentOutput],
  };
  render(<WorkflowNodeEvidence node={consumer} unitId="recorded-unit" snapshot={snapshot}
    navigation={{ checkout: testCheckout, openWorkspace: vi.fn(), openActivity: vi.fn() }} />);
  const user = userEvent.setup();
  const older = within(screen.getByText("older-output").closest("section")!);
  const result = {
    ...scope, direction: "output", producerRevision: consumer.revision, current: false,
    artifact: exactOutput.artifact, inputReceipts: exactOutput.inputReceipts,
  };
  fireEvent.contextMenu(older.getByText("older-output"));
  await user.click(await screen.findByRole("menuitem", { name: "Copy output evidence JSON" }));
  expect(JSON.parse(await navigator.clipboard.readText())).toEqual(result);

  fireEvent.contextMenu(older.getByText("older-output"));
  await user.click(await screen.findByRole("menuitem", { name: "Download output evidence (.json)" }));
  const [filename, blob] = vi.mocked(downloadArtifact).mock.calls[0]!;
  expect(filename).toBe("workflow-output-older-output.json");
  expect(blob.type).toBe("application/json");
  const downloaded = await new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = reject;
    reader.readAsText(blob);
  });
  expect(JSON.parse(downloaded)).toEqual(result);

  await user.click(older.getByText("Input evidence · Plan earlier-plan"));
  fireEvent.contextMenu(older.getByText("older-source"));
  await user.click(await screen.findByRole("menuitem", { name: "Copy input evidence JSON" }));
  const input = {
    ...scope, direction: "input", producerRevision: producer.revision, current: false, artifact: exactInput.artifact,
    input: {
      nodeInstanceId: consumer.instance.id, nodeRevision: consumer.revision, unitId: "recorded-unit",
      port: "document", artifactId: exactInput.artifact.id,
      inputReceipt: { executionKey: "earlier-plan", revision: "exact-input-receipt" },
    },
  };
  expect(JSON.parse(await navigator.clipboard.readText())).toEqual(input);

  fireEvent.contextMenu(older.getByText("unknown-artifact"));
  await user.click(await screen.findByRole("menuitem", { name: "Copy input evidence JSON" }));
  expect(JSON.parse(await navigator.clipboard.readText())).toEqual({
    ...scope, direction: "input", artifact: null,
    input: { ...input.input, port: "unavailable", artifactId: "unknown-artifact" },
  });
});
