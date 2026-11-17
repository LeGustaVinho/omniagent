// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ModelOption } from "@/bindings/ModelOption";
import { SideFixtureEvidence } from "@/capability-side-validation";
import { ModelPicker } from "@/components/model-picker";
import { ConversationProfile, type DispatchProfileEvidence } from "@/conversation-profile";
import { ModelCapabilityPreview } from "@/model-capability-preview";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const scope = { projectId: "project", conversationId: "conversation", runId: "source", live: false };
const model: ModelOption = {
  selection: { provider: "codex", profileId: "account", modelId: "gpt-6-luna" },
  name: "Luna", rating: null, selectable: true, unavailableReason: null, catalogVersion: 1,
};
const dispatch: DispatchProfileEvidence = {
  dispatchId: "dispatch", snapshotId: "snapshot", turnId: "turn", delivery: "submitted",
  profile: {
    provider: "codex", profileId: "account", cliVersion: "2.1", runner: "windows-direct",
    checkoutId: "checkout", checkoutPath: "C:\\repo", mode: "read_only", permissionRevision: "read-only",
    requestedModel: model.selection.modelId, requestedEffort: "low", reportedAtDispatch: { model: model.selection.modelId, effort: "low" },
  },
  assessment: {
    version: 2, transport: "codex_app_server", executableRevision: "executable",
    configurationRevision: "saved-config", providerRevision: null,
    capabilities: [{
      descriptor: { subject: { kind: "protocol", capability: "side_request" }, source: "provider",
        availability: "available", observed: null, blocker: null, boundary: "exact_checkout" },
      status: "reported", evidence: null,
    }, {
      descriptor: { subject: { kind: "protocol", capability: "workspace_write" }, source: "policy",
        availability: "unavailable", observed: null, blocker: "read_only_profile", boundary: "exact_checkout" },
      status: "blocked", evidence: null,
    }],
  },
  modelObservations: [],
};

function fixture(sourceRunId: string, id: string) {
  return {
    job: { id, queueState: "queued", latestAttempt: { runId: `${id}-run`, status: "succeeded" } },
    provider: "codex", profileId: "account", model: model.selection.modelId, purpose: "validation",
    sourceRunId, evidenceId: `${id}-evidence`,
    evidenceCheckedAt: "2026-11-16T13:45:00Z",
    response: { outcome: { kind: "answered", route: "fresh_codex_session", text: "code" } },
  };
}

function client() {
  return new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
}

it("queues validation for the recorded side capability and preserves the dispatch status", async () => {
  let queued = false;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "conversation_profile_evidence": return dispatch;
      case "catalog_model_ratings": return {};
      case "enqueue_conversation_side_request":
        expect(args).toEqual({
          conversationId: scope.conversationId, expectedContextId: null, expectedSourceRunId: scope.runId,
          selection: model.selection, effort: "low", mode: "read_only", purpose: "validation", question: "",
        });
        queued = true;
        return { status: "queued", jobId: "validation" };
      case "conversation_side_requests":
        expect(args).toEqual({ conversationId: scope.conversationId });
        return queued ? [fixture(scope.runId, "validation"), fixture("other-source", "foreign")] : [];
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const user = userEvent.setup();
  render(<QueryClientProvider client={client()}><ConversationProfile {...scope} /></QueryClientProvider>);
  await user.click(screen.getByText("Profile and capabilities"));
  await user.click(await screen.findByText("side request · reported"));
  await user.click(screen.getByRole("button", { name: "Validate capability" }));
  const validation = within(screen.getByRole("region", { name: "Side request capability validation" }));
  expect(await validation.findByText("Passed")).toBeTruthy();
  expect(validation.getByText("validation-evidence")).toBeTruthy();
  expect(validation.getByText("2026-11-16T13:45:00Z")).toBeTruthy();
  expect(validation.getByText("fresh codex session")).toBeTruthy();
  expect(validation.queryByText("foreign-evidence")).toBeNull();
  expect(screen.getByText("Verification status · reported")).toBeTruthy();
});

it("opens only the fixture attempt that produced the selected evidence", async () => {
  vi.mocked(invoke).mockResolvedValue([fixture("source", "proof"), fixture("unrelated", "other-proof")]);
  const user = userEvent.setup();
  render(<QueryClientProvider client={client()}><SideFixtureEvidence conversationId={scope.conversationId}
    attemptId="proof-run" evidenceId="proof-evidence" /></QueryClientProvider>);
  await user.click(screen.getByText("View fixture evidence"));
  expect(await screen.findByText("proof-evidence")).toBeTruthy();
  expect(screen.queryByText("other-proof-evidence")).toBeNull();
});

it("queues the selected managed audio fixture and previews only its authorized result", async () => {
  const queuedRecord = fixture(scope.runId, "managed-audio");
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "conversation_profile_evidence": return dispatch;
      case "catalog_model_ratings": return {};
      case "enqueue_conversation_side_request":
        expect(args).toMatchObject({ expectedSourceRunId: scope.runId, purpose: "validation", outputFixture: "audio_generation" });
        return { status: "queued", jobId: queuedRecord.job.id };
      case "conversation_side_requests": return [{
        ...queuedRecord, outputFixture: "audio_generation", evidenceId: null,
        job: { ...queuedRecord.job, projectId: scope.projectId },
        response: { ...queuedRecord.response, outputFixture: {
          tool: "omniagent_validate_output", revision: 1, providerCallId: "cli-call",
          result: { kind: "audio_generation", voice: "Windows voice", phrase: "Verification complete.", byteLen: 9000, sha256: "saved-sha" },
        } },
      }];
      case "conversation_output_fixture_audio":
        expect(args).toEqual({ projectId: scope.projectId, conversationId: scope.conversationId, runId: queuedRecord.job.latestAttempt.runId });
        return "saved-wave-data";
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const user = userEvent.setup();
  render(<QueryClientProvider client={client()}><ConversationProfile {...scope} /></QueryClientProvider>);
  await user.click(screen.getByText("Profile and capabilities"));
  await user.click(await screen.findByText("side request · reported"));
  await user.selectOptions(screen.getByLabelText("Capability to validate"), "audio_generation");
  expect(screen.getByText(/Generate the fixed verification phrase/)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Validate capability" }));
  expect(await screen.findByText("OmniAgent-managed Windows tool")).toBeTruthy();
  expect(screen.getByText("cli-call")).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "conversation_output_fixture_audio")).toBe(false);
  await user.click(screen.getByText("Preview generated audio"));
  expect((await screen.findByLabelText("Generated audio")).getAttribute("src")).toBe("data:audio/wav;base64,saved-wave-data");
});

it("opens capability evidence only for the model and profile shown in the picker", async () => {
  vi.mocked(invoke).mockResolvedValue(dispatch);
  const alternative = { ...model, name: "Luna (other profile)", selection: { ...model.selection, profileId: "other-account" } };
  const openEvidence = vi.fn();
  const user = userEvent.setup();
  render(<QueryClientProvider client={client()}>
    <ModelPicker options={[model, alternative]} label="Model"
      selection={{ mode: "single", selected: model.selection, onChange: vi.fn() }}
      previewDetails={(option) => <ModelCapabilityPreview option={option} scope={scope} openEvidence={openEvidence} />} />
  </QueryClientProvider>);
  await user.click(screen.getByRole("button", { name: "Model · Luna" }));
  const menu = within(await screen.findByRole("menu", { name: "Model" }));
  const evidence = await menu.findByRole("menuitem", { name: "Inspect capability evidence" });
  expect(evidence.getAttribute("aria-description")).toContain("1 reported · 1 blocked");
  await user.hover(menu.getByRole("menuitemradio", { name: alternative.name }));
  expect(await menu.findByRole("img", { name: "No recorded capability evidence for this model" })).toBeTruthy();
  expect(menu.queryByRole("menuitem", { name: "Inspect capability evidence" })).toBeNull();
  await user.hover(menu.getByRole("menuitemradio", { name: model.name }));
  await user.click(await menu.findByRole("menuitem", { name: "Inspect capability evidence" }));
  expect(openEvidence).toHaveBeenCalledOnce();
});
