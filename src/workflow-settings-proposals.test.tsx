// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { defaultAppearance } from "@/settings-appearance";
import SettingsDialog from "@/settings-dialog";
import { WorkflowSettingsProposals } from "@/workflow-settings-proposals";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const ipc = vi.mocked(invoke);
const checkout = { projectId: "project", checkoutId: "main", checkoutRoot: "D:/Projects/main", repositoryRoot: "D:/Projects/main" };
const preview = { ...checkout, checkoutId: "preview", checkoutRoot: "D:/Projects/preview" };
const other = { projectId: "elsewhere", checkoutId: "other", checkoutRoot: "D:/Other/unrelated", repositoryRoot: "D:/Other/unrelated" };
const templates = [{ templateId: "review-template", name: "Review" }];
const actor = { projectId: checkout.projectId, checkoutId: "preview", runId: "originating-run", dispatchId: "actual-dispatch", turnId: "turn" };
const time = 1_800_000_000;
const timeout = { field: "live_question_timeout", before: { revision: 4, value: 10 }, after: 20 };
const label = { field: "template_display_label", templateId: "review-template", templateRevision: "template-r3", before: { revision: 1, value: null }, after: "Release review" };
const pending = { id: "mixed", actor, requestId: "mixed-request", proposedAt: time, allowlistRevision: 5, reviewRevision: "mixed-r1", diffs: [timeout, label], decision: { status: "pending" } };
const declined = { ...pending, id: "denied", requestId: "denied-request", reviewRevision: "denied-r1", diffs: [timeout] };

function mount(open = vi.fn()) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><WorkflowSettingsProposals checkout={checkout} onOpenRun={open} /></QueryClientProvider>);
  return client;
}

function proposalDetails(summary: HTMLElement) {
  return within(summary.closest("details")!);
}

it("applies the complete reviewed revision, rejects a separate request, and keeps original evidence", async () => {
  const open = vi.fn();
  let current = [pending, declined];
  ipc.mockImplementation(async (command, raw) => {
    const args = raw as Record<string, unknown> | undefined;
    switch (command) {
      case "settings_patch_allowlist": return { revision: 5, grants: [] };
      case "settings_proposals": return current;
      case "registered_checkouts": return [checkout, preview, other];
      case "list_conversation_templates": return templates;
      case "review_settings_proposal": {
        const reviewed = current.find((item) => item.id === args?.proposalId)!;
        const decision = args?.accept ? { status: "applied", source: "desktop_user", allowlistRevision: 6, at: time + 60, revisions: [
          { field: "live_question_timeout", templateId: null, beforeRevision: 4, afterRevision: 5 },
          { field: "template_display_label", templateId: "review-template", beforeRevision: 1, afterRevision: 2 },
        ] } : { status: "rejected", source: "desktop_user", allowlistRevision: 6, at: time + 60 };
        const saved = { ...reviewed, decision };
        current = current.map((item) => item.id === saved.id ? saved : item);
        return saved;
      }
      default: throw Error(`Unexpected command ${command}`);
    }
  });
  mount(open);
  const user = userEvent.setup();
  const summary = await screen.findByText(/Awaiting review · 2 fields/);
  await user.click(summary);
  const mixed = proposalDetails(summary);
  expect(mixed.getByText("10 min")).toBeTruthy();
  expect(mixed.getByText("20 min")).toBeTruthy();
  expect(mixed.getByText("Default template name")).toBeTruthy();
  expect(mixed.getByText("Release review")).toBeTruthy();
  expect(mixed.getByText("template-r3")).toBeTruthy();
  expect(mixed.getByText("actual-dispatch")).toBeTruthy();
  await user.click(mixed.getByRole("button", { name: "Open originating run" }));
  expect(open).toHaveBeenCalledWith(actor.runId);
  await user.click(mixed.getByRole("button", { name: "Apply all changes" }));
  expect(ipc).toHaveBeenCalledWith("review_settings_proposal", { projectId: "project", proposalId: "mixed", expectedReviewRevision: "mixed-r1", accept: true });
  expect(await mixed.findByText(/Applied by desktop user · Allowlist revision 6/)).toBeTruthy();
  expect(mixed.getByText("Before · revision 1")).toBeTruthy();
  expect(mixed.getByText("Applied · revision 2")).toBeTruthy();
  expect(mixed.queryByRole("button", { name: "Apply all changes" })).toBeNull();

  const otherSummary = screen.getByText(/Awaiting review · 1 field/);
  await user.click(otherSummary);
  const otherReview = proposalDetails(otherSummary);
  await user.click(otherReview.getByRole("button", { name: "Reject proposal" }));
  expect(ipc).toHaveBeenCalledWith("review_settings_proposal", { projectId: "project", proposalId: "denied", expectedReviewRevision: "denied-r1", accept: false });
  expect(await otherReview.findByText(/Rejected by desktop user/)).toBeTruthy();
});

it("writes explicit checkout and template grants against the reviewed allowlist and recovers from a stale one", async () => {
  const timeoutGrant = { field: "live_question_timeout", checkoutId: null, templateId: null };
  const savedGrant = { field: "template_display_label", checkoutId: "preview", templateId: "review-template" };
  let saved = { revision: 7, grants: [timeoutGrant] as { field: string; checkoutId: string | null; templateId: string | null }[] };
  ipc.mockImplementation(async (command, raw) => {
    const args = raw as { grants: typeof saved.grants } | undefined;
    switch (command) {
      case "registered_checkouts": return [checkout, preview, other];
      case "list_conversation_templates": return templates;
      case "settings_proposals": return [];
      case "settings_patch_allowlist": return saved;
      case "set_settings_patch_allowlist":
        if (saved.revision === 7) {
          saved = { revision: 8, grants: [] };
          throw Error("The allowlist changed. Review its current revision.");
        }
        saved = { revision: 9, grants: args?.grants ?? [] };
        return saved;
      default: throw Error(`Unexpected command ${command}`);
    }
  });
  mount();
  const user = userEvent.setup();
  const scope = within(await screen.findByRole("region", { name: "Allowlisted fields" }));
  expect(await scope.findByText("Saved revision 7")).toBeTruthy();
  expect(await scope.findByRole("option", { name: "preview" })).toBeTruthy();
  expect(scope.queryByRole("option", { name: "unrelated" })).toBeNull();
  await user.selectOptions(scope.getByRole("combobox", { name: "Setting field" }), "template_display_label");
  await user.selectOptions(scope.getByRole("combobox", { name: "Checkout scope" }), "preview");
  await user.selectOptions(scope.getByRole("combobox", { name: "Template scope" }), "review-template");
  await user.click(scope.getByRole("button", { name: "Add grant" }));
  expect(ipc.mock.calls.filter(([command]) => command === "set_settings_patch_allowlist")).toHaveLength(0);
  await user.click(scope.getByRole("button", { name: "Save allowlist" }));
  expect(ipc).toHaveBeenCalledWith("set_settings_patch_allowlist", { projectId: "project", expectedRevision: 7, grants: [timeoutGrant, savedGrant] });
  expect(await scope.findByText("The saved allowlist changed. Load the latest revision before editing it.")).toBeTruthy();
  expect(scope.getByRole<HTMLButtonElement>("button", { name: "Save allowlist" }).disabled).toBe(true);
  expect(scope.getByText("D:/Projects/preview · Review")).toBeTruthy();
  await user.click(scope.getByRole("button", { name: "Load latest allowlist" }));
  expect(scope.getByText("All proposals require review.")).toBeTruthy();
  await user.click(scope.getByRole("button", { name: "Add grant" }));
  await user.click(scope.getByRole("button", { name: "Save allowlist" }));
  await waitFor(() => expect(ipc).toHaveBeenLastCalledWith("set_settings_patch_allowlist", { projectId: "project", expectedRevision: 8, grants: [savedGrant] }));
  expect(await scope.findByText("Saved revision 9")).toBeTruthy();
});

it("opens project proposal review through Workflow automation settings", async () => {
  ipc.mockImplementation(async (command) => {
    switch (command) {
      case "appearance_settings": return defaultAppearance;
      case "registered_checkouts": return [checkout, preview];
      case "list_conversation_templates": return templates;
      case "settings_patch_allowlist": return { revision: 0, grants: [] };
      case "settings_proposals": return [pending];
      default: throw Error(`Unexpected command ${command}`);
    }
  });
  const onOpenChange = vi.fn();
  const openActivity = vi.fn();
  const unused = () => undefined;
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <SettingsDialog checkout={checkout} open initialPage="automation" simulation={null} onOpenChange={onOpenChange}
      openActivity={openActivity} openChanges={unused} openWorkspace={unused} startSimulation={unused} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  const summary = await screen.findByText(/Awaiting review · 2 fields/);
  await user.click(summary);
  await user.click(proposalDetails(summary).getByRole("button", { name: "Open originating run" }));
  expect(ipc).toHaveBeenCalledWith("settings_proposals", { projectId: "project", offset: 0 });
  expect(onOpenChange).toHaveBeenCalledWith(false);
  expect(openActivity).toHaveBeenCalledWith(actor.runId);
});
