// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { WorkflowMaintenanceDelivery, WorkflowMaintenanceSettings } from "@/workflow-maintenance";
import { exampleCheckout } from "@/workbench-contract";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
afterEach(() => { cleanup(); invoke.mockReset(); });

const projectId = exampleCheckout.projectId;
const runningBuild = { version: "0.1.0", sourceCommit: "running-source", sourceModified: false, target: "windows-x64", profile: "release" };
const configuration = {
  sourceCheckoutId: exampleCheckout.checkoutId, workflowId: "maintenance-workflow",
  build: { executable: "C:/Build/builder.exe", arguments: ["--release"], timeoutSeconds: null, packagePath: "target/setup.exe", versionPath: "target/version" },
  requiredChecks: ["Windows test"], activation: "manual_fixture",
};
const settings = { revision: "settings-revision", configuration, buildExecutableRevision: "builder-revision" };
const mount = (node: ReactNode) => render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
  {node}
</QueryClientProvider>);

it("configures the exact registered checkout, workflow, build gate and activation mode", async () => {
  let saved: typeof settings | null = null;
  invoke.mockImplementation(async (command) => {
    switch (command) {
      case "workflow_maintenance": return { runningBuild, settings: saved, handoffs: [] };
      case "workflow_authoring_bases": return [{ version: { id: "maintenance-workflow" }, name: "Maintenance delivery" }];
      case "registered_checkouts": return [exampleCheckout, { ...exampleCheckout, projectId: "foreign", checkoutRoot: "D:/Foreign" }];
      case "set_workflow_maintenance": saved = settings; return settings;
      default: throw new Error(command);
    }
  });
  mount(<WorkflowMaintenanceSettings checkout={exampleCheckout} />);
  const user = userEvent.setup();
  await user.click(screen.getByText("Maintenance workflow"));
  await user.selectOptions(await screen.findByLabelText("Delivery workflow"), "maintenance-workflow");
  expect(screen.queryByRole("option", { name: "D:/Foreign" })).toBeNull();
  await user.type(screen.getByLabelText("Build executable"), configuration.build.executable);
  await user.type(screen.getByLabelText("Build arguments, one per line"), "--release");
  await user.type(screen.getByLabelText("Package path relative to source"), "target/setup.exe");
  await user.type(screen.getByLabelText("Version output path (relative UTF-8 file)"), "target/version");
  await user.type(screen.getByLabelText("Required check names, one per line"), "Windows test");
  await user.selectOptions(screen.getByLabelText("Activation after handoff"), "manual_fixture");
  await user.click(screen.getByRole("button", { name: "Save maintenance workflow" }));
  await screen.findByText("builder-revision");
  expect(invoke).toHaveBeenCalledWith("set_workflow_maintenance", { projectId, configuration, expectedRevision: null });
});

it("builds from the accepted commit and records activation of the exact handed-off digest", async () => {
  const handoff = { id: "handoff-one", workflowRunId: "run", nodeId: "activate-maintenance", gitArtifactId: "commit-artifact",
    taskId: "task", commitEvidenceId: "verified-commit",
    settingsRevision: settings.revision, version: "0.2.0", sourceRevision: "approved-source", sourceTree: "accepted-tree",
    candidateRevision: "pinned-candidate", sha256: "verified-package-sha256", packageName: "setup.exe",
    verificationId: "verified-checks", approvalArtifactId: "approval-evidence", approvalSource: "operator",
    reviewArtifactId: "review-evidence", acceptanceArtifactId: "acceptance-evidence", checks: ["Windows test"],
    activation: null as null | { id: string; recordedAt: string; workflowArtifactId: string } };
  let handoffs: (typeof handoff)[] = [];
  invoke.mockImplementation(async (command) => {
    switch (command) {
      case "workflow_maintenance": return { runningBuild, settings, handoffs };
      case "verified_delivery_commit": return { sha: "approved-source" };
      case "create_maintenance_handoff": handoffs = [handoff]; return handoff;
      case "activate_maintenance_fixture":
        handoffs = [{ ...handoff, activation: { id: "activated-fixture", recordedAt: "fixture-time", workflowArtifactId: "activation-artifact" } }];
        return handoffs[0];
      default: throw new Error(command);
    }
  });
  mount(<WorkflowMaintenanceDelivery projectId={projectId} workflowId="maintenance-workflow"
    workflowRunId="run" nodeId="activate-maintenance" gitArtifactId="commit-artifact" taskId="task" commitEvidenceId="verified-commit" />);
  const user = userEvent.setup();
  await user.click(screen.getByText("Maintenance package"));
  expect(await screen.findByText("running-source")).toBeTruthy();
  expect(await screen.findByText("approved-source")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("verified_delivery_commit", { projectId, taskId: "task", evidenceId: "verified-commit" });
  const build = screen.getByRole<HTMLButtonElement>("button", { name: "Build accepted package" });
  await waitFor(() => expect(build.disabled).toBe(false));
  await user.click(build);
  expect(invoke).toHaveBeenCalledWith("create_maintenance_handoff", {
    request: { projectId, workflowRunId: "run", nodeId: "activate-maintenance", gitArtifactId: "commit-artifact",
      taskId: "task", commitEvidenceId: "verified-commit", expectedSettingsRevision: settings.revision },
  });
  expect(await screen.findByText("verified-package-sha256")).toBeTruthy();
  expect(screen.getByText("approval-evidence")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Record fixture activation" }));
  expect(await screen.findByText("activated-fixture")).toBeTruthy();
  expect(screen.getByText("activation-artifact")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("activate_maintenance_fixture", {
    request: { projectId, handoffId: handoff.id, expectedSettingsRevision: settings.revision, expectedSha256: handoff.sha256 },
  });
});

it("verifies the signed handoff before confirmation and reports a failed installer launch", async () => {
  const handoff = {
    id: "signed-handoff", workflowRunId: "run", nodeId: "activate-maintenance", gitArtifactId: "commit-artifact",
    commitEvidenceId: "verified-commit", settingsRevision: settings.revision, version: "0.2.0",
    sourceRevision: "approved-source", sha256: "signed-package-digest", checks: ["Windows test"],
    approvalSource: "operator", packageName: "setup.exe", activation: null,
  };
  const request = {
    projectId, handoffId: handoff.id, expectedSettingsRevision: settings.revision, expectedSha256: handoff.sha256,
  };
  invoke.mockImplementation(async (command) => {
    switch (command) {
      case "workflow_maintenance": return {
        runningBuild, settings: { ...settings, configuration: { ...configuration, activation: "signed_updater" } }, handoffs: [handoff],
      };
      case "verified_delivery_commit": return { sha: handoff.sourceRevision };
      case "preview_signed_update": return { version: handoff.version, currentVersion: runningBuild.version };
      case "activate_signed_update": return { outcome: { status: "install_failed", reason: "Windows installer did not start" } };
      default: throw new Error(command);
    }
  });
  mount(<WorkflowMaintenanceDelivery projectId={projectId} workflowId="maintenance-workflow"
    workflowRunId="run" nodeId="activate-maintenance" gitArtifactId="commit-artifact" taskId="task" commitEvidenceId="verified-commit" />);
  const user = userEvent.setup();
  await user.click(screen.getByText("Maintenance package"));
  const verify = await screen.findByRole("button", { name: "Verify signed update" });
  expect(screen.queryByRole("button", { name: "Install verified update" })).toBeNull();
  await user.click(verify);
  expect(await screen.findByText(/Signature verified for 0\.2\.0/)).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("preview_signed_update", { request });
  expect(invoke.mock.calls.some(([command]) => command === "activate_signed_update")).toBe(false);
  await user.click(screen.getByRole("button", { name: "Install verified update" }));
  expect((await screen.findByRole("alert")).textContent).toContain("Windows installer did not start");
  expect(invoke).toHaveBeenCalledWith("activate_signed_update", { request });
});
