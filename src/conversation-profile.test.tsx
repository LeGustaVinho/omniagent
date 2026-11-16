// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationProfile, type DispatchProfileEvidence } from "@/conversation-profile";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("scopes inspection to the run and keeps later model evidence separate from the dispatch assessment", async () => {
  const requested = { projectId: "project", conversationId: "conversation", runId: "run" };
  const dispatch: DispatchProfileEvidence = {
    dispatchId: "dispatch", snapshotId: "snapshot", turnId: "turn", delivery: "submitted",
    profile: {
      provider: "codex", profileId: "test-profile", cliVersion: "2", runner: "windows-direct", checkoutId: "checkout",
      checkoutPath: "C:\\review", mode: "read_only", permissionRevision: "read-only-1", requestedModel: "requested-model",
      requestedEffort: "medium", reportedAtDispatch: { model: "original-model", effort: "medium" },
    },
    assessment: {
      version: 2, transport: "codex_app_server", executableRevision: "executable", configurationRevision: "original-config", providerRevision: null,
      capabilities: [{
        descriptor: { subject: { kind: "protocol", capability: "reasoning" }, source: "provider", availability: "available", observed: null, blocker: null, boundary: "exact_checkout" },
        status: "verified",
        evidence: { id: "saved-verification", checkedAt: "2026-01-01T00:00:00Z", origin: "authenticated_fixture", result: "succeeded", artifactId: "saved-artifact" },
      }],
    },
    modelObservations: [{
      eventSequence: "27", reported: { model: "later-model", effort: "high" },
      reassessment: {
        eventSequence: "28",
        assessment: {
          version: 2, transport: "codex_app_server", executableRevision: "executable", configurationRevision: "later-config", providerRevision: null,
          capabilities: [{
            descriptor: { subject: { kind: "protocol", capability: "reasoning" }, source: "provider", availability: "available", observed: null, blocker: null, boundary: "exact_checkout" },
            status: "reported", evidence: null,
          }],
        },
      },
    }],
  };
  vi.mocked(invoke).mockResolvedValue(dispatch);
  const client = new QueryClient();
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}><ConversationProfile {...requested} live={false} /></QueryClientProvider>);
  expect(invoke).not.toHaveBeenCalled();
  await user.click(screen.getByText("Profile and capabilities"));
  expect(invoke).toHaveBeenCalledWith("conversation_profile_evidence", requested);

  const original = within(await screen.findByRole("region", { name: "Effective profile at dispatch" }));
  expect(original.getByText("requested-model")).toBeTruthy();
  expect(original.getByText("original-model")).toBeTruthy();
  expect(original.getByText("original-config")).toBeTruthy();
  await user.click(original.getByText("reasoning · verified"));
  expect(original.getByText(/saved-verification/)).toBeTruthy();

  const later = within(screen.getByRole("region", { name: "Provider model reports after dispatch" }));
  expect(later.getByText(/later-model · Effort high · Event 27/)).toBeTruthy();
  await user.click(later.getByText("Assessment after report · Event 28"));
  expect(later.getByText("later-config")).toBeTruthy();
  expect(later.getByText("reasoning · reported")).toBeTruthy();
  expect(later.queryByText(/saved-verification/)).toBeNull();
});
