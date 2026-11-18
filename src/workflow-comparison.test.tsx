// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { WorkflowComparisons } from "@/workflow-comparison";
import { comparisonOutcome } from "@/workflow-comparison-outcomes";
import { DeliveryAgent } from "@/repository-delivery-agent";
import type { Job } from "@/bindings/Job";
import { testCheckout } from "@/test-fixtures/checkout";
import PipelinesWorkspace from "@/pipelines-workspace";
import { workflowComparisonNoteTarget } from "@/workflow-comparison-note-target";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const commit = "a".repeat(40);
const profiles = ["first-codex", "second-codex"].map((modelId) => ({ provider: "codex", profileId: "local", modelId }));
const options = profiles.map((selection, index) => ({
  option: { selection, name: index ? "Second Codex model" : "First Codex model", selectable: true,
    rating: { intelligenceTenths: 558, apiReferenceCents: 125, priceScore: 43 } }, efforts: [], defaultEffort: null,
}));

function mount(target?: string, fromNavigation = false) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const Surface = fromNavigation ? PipelinesWorkspace : WorkflowComparisons;
  return render(<QueryClientProvider client={client}><Surface checkout={testCheckout} target={target}
      openWorkspace={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} /></QueryClientProvider>);
}

it("pins the selected Git revision and preserves selection order for same-provider candidates", async () => {
  let groups: unknown[] = [];
  vi.mocked(invoke).mockImplementation(async (name, args) => {
    const request = args as { provider?: string; prompt?: string } | undefined;
    if (name === "conversation_models") return request?.provider === "codex" ? options : [];
    if (name === "comparison_source_commit") return commit;
    if (name === "model_comparisons") return groups;
    if (name === "start_model_comparison") {
      const row = { id: "comparison", commit, prompt: request?.prompt, tree: "git-tree",
        candidates: [profiles[1], profiles[0]].map((selection, index) => ({ selection, jobId: `job-${index}`, state: "queued" })) };
      groups = [row];
      return row;
    }
    throw Error(`Unexpected IPC ${name}`);
  });
  const user = userEvent.setup();
  const view = mount();
  await user.type(screen.getByRole("textbox", { name: "Shared request" }), "Review the changes");
  await user.click(screen.getByRole("button", { name: /Comparison candidates/ }));
  await user.click(await screen.findByRole("menuitemcheckbox", { name: /Second Codex model/ }));
  await user.click(screen.getByRole("menuitemcheckbox", { name: /First Codex model/ }));
  await user.keyboard("{Escape}");
  const order = within(screen.getByRole("list", { name: "Candidate order" }));
  expect(order.getAllByRole("listitem").map((item) => item.querySelector("strong")?.textContent))
    .toEqual(["Second Codex model", "First Codex model"]);
  expect(order.getAllByRole("meter", { name: "Intelligence" })).toHaveLength(2);
  expect(order.getAllByText(/\$1.25 \/ 1M tokens/)).toHaveLength(2);
  await user.click(screen.getByRole("button", { name: "Start comparison" }));
  expect(invoke).toHaveBeenCalledWith("start_model_comparison", {
    projectId: testCheckout.projectId, checkoutId: testCheckout.checkoutId, commit,
    prompt: "Review the changes", selections: [profiles[1], profiles[0]],
  });
  expect(within(await screen.findByRole("list", { name: "Comparison results" })).getAllByRole("status").map((status) => status.textContent))
    .toEqual(["queued", "queued"]);
  view.unmount();
  mount();
  expect(within(await screen.findByRole("list", { name: "Comparison results" })).getAllByRole("heading").map((heading) => heading.textContent))
    .toEqual(["second-codex", "first-codex"]);
});

it("routes from Knowledge through Pipelines to a comparison pinned to the reviewed saved revision", async () => {
  const { projectId, checkoutId } = testCheckout;
  const revision = { id: "saved-revision", checkoutId, relativePath: "notes/Decisão técnica.md", sha256: "b".repeat(64), baseCommit: commit };
  const markdown = "# Pinned requirement\r\nOriginal constraints\r\n";
  const target = workflowComparisonNoteTarget({ checkoutId, revisionId: revision.id, relativePath: revision.relativePath, sha256: revision.sha256 });
  const source = { revision, markdown };
  vi.mocked(invoke).mockImplementation(async (name, args) => {
    if (name === "conversation_models") return (args as { provider: string }).provider === "codex" ? options : [];
    if (name === "read_repository_markdown") return source;
    if (name === "model_comparisons") return [];
    if (name === "start_model_comparison_from_markdown") return { id: "comparison-note", sourceCheckoutId: checkoutId,
      sourceDocument: revision, commit, prompt: "Evaluate feasibility", tree: "tree", candidates: [] };
    throw Error(`Unexpected IPC ${name}`);
  });
  mount(target, true);
  const user = userEvent.setup();
  const pinned = within(await screen.findByLabelText("Saved note input"));
  expect(pinned.getByText(revision.relativePath)).toBeTruthy();
  expect(pinned.getByText(revision.id)).toBeTruthy();
  expect(pinned.getByText(revision.sha256)).toBeTruthy();
  expect(pinned.getByText(commit)).toBeTruthy();
  expect(screen.queryByRole("textbox", { name: "Git commit" })).toBeNull();
  await user.click(screen.getByText("Read saved Markdown"));
  expect(screen.getByText(/Pinned requirement/, { selector: "pre" }).textContent).toBe(markdown);
  expect(invoke).toHaveBeenCalledWith("read_repository_markdown", { checkoutId, revisionId: revision.id });
  expect(invoke).not.toHaveBeenCalledWith("comparison_source_commit", expect.anything());
  await user.click(screen.getByRole("button", { name: /Comparison candidates/ }));
  await user.click(await screen.findByRole("menuitemcheckbox", { name: /Second Codex model/ }));
  await user.click(screen.getByRole("menuitemcheckbox", { name: /First Codex model/ }));
  await user.keyboard("{Escape}");
  const launch = screen.getByRole<HTMLButtonElement>("button", { name: "Start comparison" });
  expect(launch.disabled).toBe(true);
  await user.type(screen.getByRole("textbox", { name: "Shared request" }), "  Evaluate feasibility  ");
  expect(invoke).not.toHaveBeenCalledWith("start_model_comparison_from_markdown", expect.anything());
  await user.click(launch);
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("start_model_comparison_from_markdown", {
    projectId, checkoutId, revisionId: revision.id, relativePath: revision.relativePath, sha256: revision.sha256,
    prompt: "Evaluate feasibility", selections: [profiles[1], profiles[0]],
  }));
  expect(invoke).not.toHaveBeenCalledWith("start_model_comparison", expect.anything());
  const result = within(await screen.findByRole("region", { name: "Saved comparison" }));
  expect(result.getByText(revision.id)).toBeTruthy();
});

it("does not offer a working checkout comparison for a mismatched note revision", async () => {
  const { checkoutId } = testCheckout;
  const target = workflowComparisonNoteTarget({ checkoutId, revisionId: "expected", relativePath: "Note.md", sha256: "d".repeat(64) });
  vi.mocked(invoke).mockImplementation(async (name) => {
    if (name === "conversation_models" || name === "model_comparisons") return [];
    if (name === "read_repository_markdown") return { markdown: "A different note", revision: {
      checkoutId, id: "expected", relativePath: "Somewhere else.md", sha256: "d".repeat(64), baseCommit: commit,
    } };
    throw Error(`Unexpected IPC ${name}`);
  });
  mount(target);
  expect((await screen.findByRole("alert")).textContent).toBe("The chosen Markdown revision is unavailable.");
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Start comparison" }).disabled).toBe(true);
  expect(screen.queryByText("A different note")).toBeNull();
  expect(invoke).not.toHaveBeenCalledWith("comparison_source_commit", expect.anything());
});

it("shows each dispatch's observed instruction reads and tool use alongside the shared revision", async () => {
  vi.mocked(invoke).mockImplementation(async (name) => {
    if (name === "conversation_models") return [];
    if (name === "comparison_source_commit") return commit;
    if (name === "model_comparisons") return [{ id: "comparison", commit, tree: "tree", prompt: "Review", candidates:
      profiles.map((selection, index) => ({ selection, jobId: `job-${index}`, dispatchId: `dispatch-${index}`, state: "succeeded" })) }];
    if (name === "model_comparison_environments") return profiles.map((_, index) => ({
      jobId: `job-${index}`, dispatchId: `dispatch-${index}`, instructions: [{ path: "AGENTS.md", scope: "checkout",
        revision: "abcdef123456", selection: "expected", observedRead: index === 0 }],
      reportedTools: [], observedTools: index === 0 ? ["Read"] : ["Search"],
    }));
    throw Error(`Unexpected IPC ${name}`);
  });
  mount();
  const user = userEvent.setup();
  const rows = within(await screen.findByRole("list", { name: "Comparison results" })).getAllByRole("listitem");
  await user.click(await within(rows[0]!).findByText("Instructions and tools"));
  await user.click(await within(rows[1]!).findByText("Instructions and tools"));
  expect(within(rows[0]!).getByText("Observed read")).toBeTruthy();
  expect(within(rows[0]!).getByText("Tools used · Read")).toBeTruthy();
  expect(within(rows[1]!).getByText("expected")).toBeTruthy();
  expect(within(rows[1]!).getByText("Tools used · Search")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("model_comparison_environments", { projectId: testCheckout.projectId, groupId: "comparison" });
});

it("keeps the original successful response visible next to a timed-out candidate", async () => {
  const measurements = {
    reportedModel: "codex-reported", inputTokens: 10, outputTokens: 7, cachedInputTokens: 2,
    reasoningTokens: 5, durationSeconds: 12, reasoning: [], failure: null, response: null,
  };
  vi.mocked(invoke).mockImplementation(async (name) => {
    if (name === "conversation_models") return [];
    if (name === "comparison_source_commit") return commit;
    if (name === "model_comparisons") return [{ id: "comparison", commit, tree: "tree", prompt: "Review", candidates:
      profiles.map((selection, index) => ({ selection, jobId: `job-${index}`, runId: `run-${index}`, state: index ? "succeeded" : "failed" })) }];
    if (name === "model_comparison_outcomes") return [
      { ...measurements, jobId: "job-1", runId: "run-1", response: "Original model answer", reasoning: [{ text: "Reported reasoning", summary: true }] },
      { ...measurements, jobId: "job-0", runId: "run-0", failure: { cause: "startup_timeout", phase: "startup" } },
    ];
    throw Error(`Unexpected IPC ${name}`);
  });
  mount();
  expect(await screen.findByRole("heading", { name: "Partial result" })).toBeTruthy();
  const rows = within(screen.getByRole("list", { name: "Comparison results" })).getAllByRole("listitem");
  expect(await within(rows[0]!).findByText(/startup timeout/)).toBeTruthy();
  expect(within(rows[0]!).queryByText("Original model answer")).toBeNull();
  expect(await within(rows[1]!).findByText("Original model answer")).toBeTruthy();
  expect(within(rows[1]!).getByText("12 s")).toBeTruthy();
  expect(within(rows[1]!).getByText("codex-reported")).toBeTruthy();
  expect(within(rows[1]!).getByText("Output tokens").nextElementSibling?.textContent).toBe("7");
  await userEvent.setup().click(within(rows[1]!).getByText("Available reasoning"));
  expect(within(rows[1]!).getByText("Reported reasoning")).toBeTruthy();
  expect(comparisonOutcome(["failed", "interrupted"])).toBe("Comparison failed");
});

it("queues a separate synthesis from only selected successful run IDs and restores its receipt", async () => {
  const outcomes = [{ jobId: "first-job", runId: "first-run", reportedModel: "first-codex", response: "Original saved answer",
    reasoning: [], failure: null, durationSeconds: 4, inputTokens: null, outputTokens: null, cachedInputTokens: null, reasoningTokens: null }];
  const group = { id: "sources", commit, tree: "tree", prompt: "Review", candidates: profiles.map((selection, i) => i === 0
    ? { selection, jobId: "first-job", runId: "first-run", state: "succeeded", dispatchId: "first-dispatch" }
    : { selection, jobId: "failed-job", runId: "failed-run", state: "failed" }) };
  const saved = { id: "synthesis", jobId: "synthesis-job", selection: profiles[1], state: "succeeded",
    runId: "synthesis-run", conversationId: "synthesis-chat", dispatchId: "synthesis-dispatch", response: "Separate synthesis answer",
    inputs: [{ groupId: group.id, jobId: "first-job", runId: "first-run", dispatchId: "first-dispatch", responseRevisionId: "source-answer" }],
  };
  let syntheses: unknown[] = [];
  vi.mocked(invoke).mockImplementation(async (name, args) => {
    if (name === "conversation_models") return args && "provider" in args && args.provider === "codex" ? options : [];
    if (name === "comparison_source_commit") return commit;
    if (name === "model_comparisons") return [group];
    if (name === "model_comparison_outcomes") return outcomes;
    if (name === "model_comparison_environments") return [];
    if (name === "model_comparison_syntheses") return syntheses;
    if (name === "start_model_comparison_synthesis") { syntheses = [saved]; return saved; }
    throw Error(`Unexpected IPC ${name}`);
  });
  const user = userEvent.setup();
  const initial = mount();
  await screen.findByText("Original saved answer");
  const candidate = await screen.findByRole("heading", { name: "first-codex" });
  fireEvent.contextMenu(candidate);
  fireEvent.click(await screen.findByRole("menuitem", { name: "Copy original response" }));
  await screen.findByText("Copied to the clipboard.");
  expect(await navigator.clipboard.readText()).toBe("Original saved answer");
  fireEvent.contextMenu(candidate);
  fireEvent.click(await screen.findByRole("menuitem", { name: "Include in synthesis" }));
  expect((await screen.findByRole<HTMLInputElement>("checkbox", { name: /first-codex.*first-run/ })).checked).toBe(true);
  expect(screen.queryByRole("checkbox", { name: /failed-run/ })).toBeNull();
  expect(invoke).not.toHaveBeenCalledWith("start_model_comparison_synthesis", expect.anything());
  await user.click(screen.getByRole("button", { name: /Synthesis model/ }));
  await user.click(await screen.findByRole("menuitemradio", { name: /Second Codex model/ }));
  await user.click(screen.getByRole("button", { name: "Queue synthesis" }));
  expect(invoke).toHaveBeenCalledWith("start_model_comparison_synthesis", {
    projectId: testCheckout.projectId, sources: { groupId: "sources", runIds: ["first-run"] }, selection: profiles[1],
  });
  expect(await screen.findByText("Separate synthesis answer")).toBeTruthy();
  expect(screen.getByText("Original saved answer")).toBeTruthy();
  initial.unmount();
  mount();
  await user.click(await screen.findByText("Synthesis", { selector: "summary" }));
  const synthesis = within(await screen.findByRole("article", { name: "Synthesis result" }));
  expect(synthesis.getByText("first-run")).toBeTruthy();
  expect(synthesis.getByText("first-dispatch")).toBeTruthy();
  expect(synthesis.getByRole("button", { name: "Open synthesis execution" })).toBeTruthy();
});

it("passes exact comparison inputs through the normal plan-review action", async () => {
  const scope = { projectId: testCheckout.projectId, runId: "workflow-run" };
  const group = { id: "comparison", commit, tree: "tree", prompt: "Check requirements", candidates: [
    { selection: profiles[0], jobId: "source-job", runId: "source-run", dispatchId: "source-dispatch", state: "succeeded" },
  ] };
  const review = { id: "review-job", queueState: "queued", latestAttempt: null } as Job;
  vi.mocked(invoke).mockImplementation(async (name, args) => {
    if (name === "conversation_models") return args && "provider" in args && args.provider === "codex" ? options : [];
    if (name === "model_comparisons") return [group];
    if (name === "model_comparison_outcomes") return [{ jobId: "source-job", runId: "source-run", response: "Checked" }];
    if (name === "submit_repository_delivery_agent") return review;
    if (name === "model_comparison_job_inputs") return [{ groupId: "comparison", jobId: "source-job", runId: "source-run",
      dispatchId: "source-dispatch", responseRevisionId: "source-response" }];
    throw Error(`Unexpected IPC ${name}`);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const renderStage = (job?: Job) => <QueryClientProvider client={client}>
    <DeliveryAgent scope={scope} nodeId="review-plan" inputs={{ plan: "pinned-plan" }} label="Plan review" job={job}
      actions={{ busy: false, execute: (operation) => { void operation(); } }} openActivity={vi.fn()} />
  </QueryClientProvider>;
  const view = render(renderStage());
  const user = userEvent.setup();
  await user.click(await screen.findByText("Use model comparison responses"));
  const source = await screen.findByRole("combobox", { name: "Source comparison" });
  await within(source).findByRole("option", { name: /Check requirements/ });
  await user.selectOptions(source, "comparison");
  await user.click(await screen.findByRole("checkbox", { name: /source-run/ }));
  await user.click(screen.getByRole("button", { name: /Plan review model/ }));
  await user.click(await screen.findByRole("menuitemradio", { name: /Second Codex model/ }));
  await user.click(screen.getByRole("button", { name: "Queue plan review" }));
  expect(invoke).toHaveBeenCalledWith("submit_repository_delivery_agent", { input: {
    ...scope, nodeId: "review-plan", unitId: null, inputs: { plan: "pinned-plan" }, selection: profiles[1], effort: null,
    comparison: { groupId: "comparison", runIds: ["source-run"] },
  } });
  view.rerender(renderStage(review));
  await user.click(screen.getByText("Comparison inputs for this attempt"));
  const linked = within(await screen.findByRole("list", { name: "Source comparison runs" }));
  expect(linked.getByText("source-run")).toBeTruthy();
  expect(linked.getByText("source-dispatch")).toBeTruthy();
});
