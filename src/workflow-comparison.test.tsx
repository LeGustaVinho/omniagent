// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { WorkflowComparisons } from "@/workflow-comparison";
import { comparisonOutcome } from "@/workflow-comparison-outcomes";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const commit = "a".repeat(40);
const profiles = ["first-codex", "second-codex"].map((modelId) => ({ provider: "codex", profileId: "local", modelId }));
const options = profiles.map((selection, index) => ({
  option: { selection, name: index ? "Second Codex model" : "First Codex model", selectable: true,
    rating: { intelligenceTenths: 558, apiReferenceCents: 125, priceScore: 43 } }, efforts: [], defaultEffort: null,
}));

function mount() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}><WorkflowComparisons checkout={exampleCheckout}
    simulation={null} startSimulation={vi.fn()} openWorkspace={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} /></QueryClientProvider>);
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
    projectId: exampleCheckout.projectId, checkoutId: exampleCheckout.checkoutId, commit,
    prompt: "Review the changes", selections: [profiles[1], profiles[0]],
  });
  expect(within(await screen.findByRole("list", { name: "Comparison results" })).getAllByRole("status").map((status) => status.textContent))
    .toEqual(["queued", "queued"]);
  view.unmount();
  mount();
  expect(within(await screen.findByRole("list", { name: "Comparison results" })).getAllByRole("heading").map((heading) => heading.textContent))
    .toEqual(["second-codex", "first-codex"]);
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
  expect(invoke).toHaveBeenCalledWith("model_comparison_environments", { projectId: exampleCheckout.projectId, groupId: "comparison" });
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
