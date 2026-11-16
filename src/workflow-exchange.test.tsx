// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { WorkflowExchangeDownload, WorkflowExchangePanel } from "@/workflow-exchange";
import type { ExchangeBinding } from "@/workflow-exchange-bindings";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@/workflow-exchange-bindings", () => ({
  WorkflowExchangeBindings: ({ onChange }: { onChange: (key: string, binding: ExchangeBinding) => void }) =>
    <button type="button" onClick={() => onChange("binding-model", { type: "model_profile",
      selection: { provider: "codex", profileId: "destination", modelId: "model" },
    })}>Use destination profile</button>,
}));

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); invoke.mockReset(); });

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
  const createObjectURL = vi.fn(() => "blob:approved-export");
  vi.stubGlobal("URL", Object.assign(class extends URL {}, { createObjectURL, revokeObjectURL: vi.fn() }));
  const filenames: string[] = [];
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
    filenames.push(this.download);
  });
  invoke.mockResolvedValue({ format: "omniagent.workflow-package", version: 1 });
  mount(<WorkflowExchangeDownload projectId="source-project" versions={[version]} />);
  await userEvent.click(screen.getByRole("button", { name: "Download package" }));
  await waitFor(() => expect(filenames).toEqual(["omniagent-workflow-package.json"]));
  expect(createObjectURL).toHaveBeenCalledWith(expect.any(Blob));
  expect(invoke).toHaveBeenCalledWith("export_workflow_exchange", { projectId: "source-project", selected: [version] });
});
