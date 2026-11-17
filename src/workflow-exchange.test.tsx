// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { downloadArtifact } from "@/download-artifact";
import { WorkflowExchangeDownload, WorkflowExchangePanel } from "@/workflow-exchange";
import type { ExchangeBinding } from "@/workflow-exchange-bindings";
import { exampleCheckout } from "@/workbench-contract";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@/download-artifact", async (original) => ({
  ...await original<typeof import("@/download-artifact")>(),
  downloadArtifact: vi.fn(),
}));
vi.mock("@/workflow-exchange-bindings", () => ({
  WorkflowExchangeBindings: ({ onChange }: { onChange: (key: string, binding: ExchangeBinding) => void }) =>
    <button type="button" onClick={() => onChange("binding-model", { type: "model_profile",
      selection: { provider: "codex", profileId: "destination", modelId: "model" },
    })}>Use destination profile</button>,
}));

afterEach(() => { cleanup(); invoke.mockReset(); vi.mocked(downloadArtifact).mockReset(); });

const version = { id: "preset", revision: "historical", kind: "preset", destination: "library" } as const;
const mount = (node: ReactNode) => render(<QueryClientProvider client={new QueryClient()}>{node}</QueryClientProvider>);

it("imports only the original document and destination bindings covered by its latest preview", async () => {
  const document = '{"format":"omniagent.workflow-package","version":1}';
  const initial = {
    name: "Fixture package", members: { preset: version }, missingBindings: ["binding-model"],
    destinationNotes: ["Earlier revisions stay available."], permissions: ["model_requires_local_authentication"],
    conflicts: [{ member: "preset", node: "scan", port: "context", message: "Check the input schema.", blocking: true }],
    bindings: {}, canImport: false, reviewDigest: "first",
  };
  invoke.mockResolvedValueOnce(initial)
    .mockResolvedValueOnce({ ...initial, missingBindings: [], conflicts: [], canImport: true, reviewDigest: "exact-preview" })
    .mockResolvedValueOnce({ id: "new-draft" });
  const openWorkspace = vi.fn();
  mount(<WorkflowExchangePanel projectId="destination-project" openWorkspace={openWorkspace} />);
  const user = userEvent.setup();
  await user.click(screen.getByText("Import or export workflow packages"));
  const file = new File([document], "fixture.json", { type: "application/json" });
  Object.defineProperty(file, "text", { value: () => Promise.resolve(document) });
  fireEvent.change(await screen.findByLabelText("Workflow package file"), { target: { files: [file] } });
  const preview = screen.getByRole<HTMLButtonElement>("button", { name: "Preview import" });
  await waitFor(() => expect(preview.disabled).toBe(false));
  await user.click(preview);
  const commit = await screen.findByRole<HTMLButtonElement>("button", { name: "Import as draft" });
  expect(commit.disabled).toBe(true);
  expect(screen.getByText(/scan · context.*Check the input schema/)).toBeTruthy();
  expect(screen.getByText("Model access uses destination profiles")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Use destination profile" }));
  expect(screen.getByText("Preview the destination again after changing bindings.")).toBeTruthy();
  expect(invoke.mock.calls).toHaveLength(1);
  await user.click(preview);
  await waitFor(() => expect(commit.disabled).toBe(false));
  await user.click(commit);
  await waitFor(() => expect(openWorkspace).toHaveBeenCalledWith("pipelines", "authoring:package:new-draft"));
  expect(invoke).toHaveBeenLastCalledWith("import_reviewed_workflow_exchange", {
    projectId: "destination-project", document, reviewedDigest: "exact-preview",
    bindings: { "binding-model": { type: "model_profile", selection: { provider: "codex", profileId: "destination", modelId: "model" } } },
  });
});

it("downloads an explicitly selected historical artifact revision", async () => {
  invoke.mockResolvedValue({ format: "omniagent.workflow-package", version: 1 });
  mount(<WorkflowExchangeDownload projectId="source-project" versions={[version]} />);
  await userEvent.click(screen.getByRole("button", { name: "Download package" }));
  await waitFor(() => expect(downloadArtifact).toHaveBeenCalledExactlyOnceWith("omniagent-workflow-package.json", expect.any(Blob)));
  expect(vi.mocked(downloadArtifact).mock.calls[0]?.[1].type).toBe("application/json");
  expect(invoke).toHaveBeenCalledWith("export_workflow_exchange", { projectId: "source-project", selected: [version] });
});

it("exports the clicked library preset revision independently of the checked package assembly", async () => {
  const other = { ...version, revision: "other-head", destination: "project" };
  const evidence = { format: "omniagent.workflow-package", version: 1 };
  invoke.mockImplementation(async (command) => {
    if (command === "workflow_package_catalog") return [{ version, name: "Shared preset" }, { version: other, name: "Shared preset" }];
    if (command === "export_workflow_exchange") return evidence;
    throw new Error(command);
  });
  mount(<WorkflowExchangePanel projectId="source-project" openWorkspace={vi.fn()} />);
  const user = userEvent.setup();
  await user.click(screen.getByText("Import or export workflow packages"));
  await user.click(screen.getByText("Export published revisions"));
  const clicked = await screen.findByRole<HTMLInputElement>("checkbox", { name: /Shared preset.*library.*historical/ });
  const checked = screen.getByRole<HTMLInputElement>("checkbox", { name: /Shared preset.*project.*other-head/ });
  await user.click(checked);
  fireEvent.contextMenu(clicked);
  await user.click(await screen.findByRole("menuitem", { name: "Download package for this revision" }));
  await waitFor(() => expect(downloadArtifact).toHaveBeenCalledTimes(1));
  expect(invoke).toHaveBeenLastCalledWith("export_workflow_exchange", { projectId: "source-project", selected: [version] });
  expect(checked.checked).toBe(true);
  expect(clicked.checked).toBe(false);
  await user.click(screen.getByRole("button", { name: "Download package" }));
  await waitFor(() => expect(downloadArtifact).toHaveBeenCalledTimes(2));
  expect(invoke).toHaveBeenLastCalledWith("export_workflow_exchange", { projectId: "source-project", selected: [other] });
});
