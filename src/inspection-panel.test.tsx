// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { RunRecord } from "@/bindings/RunRecord";
import { downloadArtifact } from "@/download-artifact";
import { RunInspector } from "@/inspection-panel";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/download-artifact", () => ({ downloadArtifact: vi.fn(), decodeArtifactBytes: vi.fn() }));
vi.mock("@/git-diff-view", () => ({ MonacoComparison: ({ file }: { file: { original: string; modified: string } }) =>
  <div><pre aria-label="Original evidence">{file.original}</pre><pre aria-label="Modified evidence">{file.modified}</pre></div> }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

function readDownload(blob: Blob) {
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = reject;
    reader.readAsText(blob);
  });
}

it("opens recorded run pages and loads exact evidence on selection", async () => {
  const conversationId = "recorded-conversation";
  const run: RunRecord = {
    id: "recorded-run", projectId: exampleCheckout.projectId, checkoutId: exampleCheckout.checkoutId,
    turnId: "recorded-turn", status: "succeeded",
  };
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "inspect_conversation_run": return [run, conversationId];
      case "checkout_conversation": return { id: conversationId, checkoutId: run.checkoutId, title: "Recorded", contextId: "source-context" };
      case "conversation_events": return { cursor: "0", more: false, events: [] };
      case "saved_conversation_questions": case "conversation_approvals": return [];
      case "conversation_checklist_progress": case "downstream_wait": return null;
      case "conversation_input": return {
        delivery: "submitted", turnId: run.turnId, frames: ["Exact recorded request"],
        attachments: [], removedAttachmentCount: 0, instructions: [],
      };
      case "run_orchestration": return { job: { taskId: null, ownerTaskId: null } };
      case "attempt_worktree_evidence": return {
        kind: "saved",
        artifact: {
          revision: "frozen-candidate", committedPatchBase64: "",
          start: { starting: { head: "baseline", changes: null }, worktree: { ownerTaskId: "task", branch: "work" } },
          candidate: { head: "saved-head", changes: null },
        },
      };
      default: throw new Error(command);
    }
  });
  const props = {
    checkout: exampleCheckout, runId: run.id, conversationId, onClose: vi.fn(),
    openWorkspace: vi.fn(), openChanges: vi.fn(), openActivity: vi.fn(),
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><RunInspector {...props} /></QueryClientProvider>);

  const activity = within(await screen.findByRole("complementary", { name: "Conversation activity" }));
  expect(activity.getAllByRole("tab").map((element) => element.textContent)).toEqual(["Overview", "Timeline", "Agents", "Context", "Exact input", "Approvals", "Artifacts"]);
  expect(activity.getByRole("tab", { name: "Overview", selected: true })).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "conversation_input")).toBe(false);
  await userEvent.click(activity.getByRole("tab", { name: "Exact input" }));
  expect(await activity.findByText("Exact recorded request")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("conversation_input", { conversationId, runId: run.id });

  await userEvent.click(activity.getByRole("tab", { name: "Approvals" }));
  expect(await activity.findByText("No approval requests recorded.")).toBeTruthy();
  expect(activity.queryByText("Exact recorded request")).toBeNull();
  expect(invoke).toHaveBeenCalledWith("conversation_approvals", { conversationId, runId: run.id });

  await userEvent.click(activity.getByRole("tab", { name: "Artifacts" }));
  await userEvent.click(await activity.findByText("Saved candidate for this attempt"));
  await userEvent.click(await activity.findByRole("button", { name: "Open saved diff in Git" }));
  expect(props.openWorkspace).toHaveBeenCalledExactlyOnceWith("git", "attempt:recorded-run");

  fireEvent.contextMenu(activity.getByText(run.id));
  await userEvent.click(screen.getByRole("menuitem", { name: "Open conversation" }));
  expect(invoke).toHaveBeenCalledWith("checkout_conversation", { checkoutId: run.checkoutId, conversationId });
  expect(props.openWorkspace).toHaveBeenLastCalledWith("conversations", conversationId);
});

it("copies captured command output and failure evidence for the exact inspected run", async () => {
  const projectId = exampleCheckout.projectId;
  const run: RunRecord = { id: "command-attempt", projectId, checkoutId: "attempt-worktree", turnId: "command-turn", status: "failed" };
  const process = { code: 17, cancelled: false };
  const stdout = "  First line\r\nSecond line — ç\r\n";
  const stderr = "Checks failed\n";
  const openActivity = vi.fn();
  const openWorkspace = vi.fn();
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "command_job_run") return run;
    if (command === "command_job_output") return (args as { runId: string }).runId === run.id
      ? { process, stdout, stderr, truncated: true } : { process: { code: 0 }, stdout: "Prior output", stderr: "", truncated: false };
    if (command === "related_saved_attempts") return [
      { runId: run.id, checkoutId: run.checkoutId, status: "failed", ordinal: 2, createdAt: 9 },
      { runId: "prior-command", checkoutId: run.checkoutId, status: "succeeded", ordinal: 1, createdAt: 5 },
    ];
    if (command === "list_conversations") return [{ id: "diagnostic-chat", title: "Investigate checks" }];
    if (command === "append_checkout_context_to_conversation") return;
    if (command === "job_failure") return { outcome: "failed", checkoutId: run.checkoutId, checkoutRoot: "D:\\worktree",
      observation: { phase: "execution", cause: "process_exit", process }, providerOutcome: null, logsAvailable: false, savedDiffAvailable: false };
    throw new Error(`Unexpected ${command}`);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><RunInspector checkout={exampleCheckout} runId={run.id}
 onClose={vi.fn()} openWorkspace={openWorkspace} openChanges={vi.fn()} openActivity={openActivity} /></QueryClientProvider>);
  const user = userEvent.setup();
  const activity = within(await screen.findByRole("region", { name: "Command activity" }));
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "command_job_output")).toBe(false);
  fireEvent.contextMenu(await activity.findByRole("heading", { name: "Failure record" }));
  expect(screen.queryByRole("menuitem", { name: "Open recorded activity" })).toBeNull();
  await user.click(screen.getByRole("menuitem", { name: "Copy failure summary" }));
  expect(await navigator.clipboard.readText()).toBe(`Run ${run.id}\nPhase\tExecution\nWorktree\tD:\\worktree\nCause\tThe provider exited without completing the attempt.\nProcess\tExited (code 17)`);

  fireEvent.contextMenu(activity.getByText(run.id));
  await user.click(screen.getByRole("menuitem", { name: "Copy run ID" }));
  expect(await navigator.clipboard.readText()).toBe(run.id);
  fireEvent.contextMenu(activity.getByText(run.id));
  fireEvent.keyDown(screen.getByRole("menuitem", { name: "Go to" }), { key: "ArrowRight" });
  fireEvent.click(await screen.findByRole("menuitem", { name: "Command output" }));
  fireEvent.contextMenu(await activity.findByText("Checks failed"));
  await user.click(screen.getByRole("menuitem", { name: "Copy standard error" }));
  expect(await navigator.clipboard.readText()).toBe(stderr);
  fireEvent.contextMenu(activity.getByRole("heading", { name: "Standard output" }));
  await user.click(screen.getByRole("menuitem", { name: "Copy captured output" }));
  expect(await navigator.clipboard.readText()).toBe(`Exit code 17\n\nOutput capture is incomplete.\n\nStandard output\n${stdout}\n\nStandard error\n${stderr}`);
  expect(activity.getByText("Output capture is incomplete.")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("command_job_output", { projectId, runId: run.id });
  expect(activity.getByText(/The JSON record includes its project, checkout and run IDs/)).toBeTruthy();
  fireEvent.contextMenu(activity.getByRole("heading", { name: "Standard output" }));
  await user.click(screen.getByRole("menuitem", { name: "Download standard output (.log)" }));
  const [outputName, outputBlob] = vi.mocked(downloadArtifact).mock.calls[0]!;
  expect(outputName).toBe("run-command-attempt-stdout-partial.log");
  expect(outputBlob.type).toBe("text/plain;charset=utf-8");
  expect(await readDownload(outputBlob)).toBe(stdout);
  fireEvent.contextMenu(activity.getByRole("heading", { name: "Standard error" }));
  await user.click(screen.getByRole("menuitem", { name: "Download standard error (.log)" }));
  const [errorName, errorBlob] = vi.mocked(downloadArtifact).mock.calls[1]!;
  expect(errorName).toBe("run-command-attempt-stderr-partial.log");
  expect(await readDownload(errorBlob)).toBe(stderr);
  fireEvent.contextMenu(activity.getByText("Exit code 17"));
  await user.click(screen.getByRole("menuitem", { name: "Download captured output record (.json)" }));
  const [recordName, recordBlob] = vi.mocked(downloadArtifact).mock.calls[2]!;
  expect(recordName).toBe("run-command-attempt-output-partial.json");
  expect(JSON.parse(await readDownload(recordBlob))).toEqual({
    projectId, checkoutId: run.checkoutId, runId: run.id, process, stdout, stderr, truncated: true,
  });

  const printedError = activity.getByText("Checks failed");
  const selection = document.getSelection()!;
  const range = document.createRange();
  range.setStart(printedError.firstChild!, 7);
  range.setEnd(printedError.firstChild!, 13);
  selection.removeAllRanges();
  selection.addRange(range);
  fireEvent.contextMenu(printedError);
  await user.click(await screen.findByRole("menuitem", { name: "Investigate selected output…" }));
  selection.removeAllRanges();
  const draft = within(await screen.findByRole("dialog", { name: "Add checkout context" }));
  expect(draft.getByLabelText("Selected text").textContent).toBe("failed");
  expect(draft.getByText("Standard error · Line 1 · Run command-attempt · Worktree attempt-worktree · Exit code 17 · Incomplete recorded output")).toBeTruthy();
  await user.selectOptions(draft.getByLabelText("Conversation"), "diagnostic-chat");
  await user.click(draft.getByRole("button", { name: "Add to draft" }));
  await waitFor(() => expect(openWorkspace).toHaveBeenCalledWith("conversations", "diagnostic-chat"));
  expect(invoke).toHaveBeenCalledWith("append_checkout_context_to_conversation", {
    checkoutId: exampleCheckout.checkoutId, conversationId: "diagnostic-chat", context: expect.stringContaining("\nfailed\n```"),
  });

  fireEvent.contextMenu(activity.getByText("Exit code 17"));
  await user.click(screen.getByRole("menuitem", { name: "Compare saved attempt output…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Compare saved attempt output" }));
  expect((await dialog.findByLabelText("Original evidence")).textContent).toBe("Standard output\nPrior output\n\nStandard error\n");
  expect(dialog.getByLabelText("Modified evidence").textContent).toBe(`Standard output\n${stdout}\n\nStandard error\n${stderr}`);
  expect(dialog.getByText("Output capture is incomplete")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("related_saved_attempts", { projectId, runId: run.id });
  await user.click(dialog.getByRole("button", { name: "Open base attempt" }));
  expect(openActivity).toHaveBeenCalledExactlyOnceWith("prior-command");
});

it("compares only recorded attempts of the inspected conversation and opens the selected provenance", async () => {
  const conversationId = "conversation";
  const projectId = exampleCheckout.projectId;
  const runId = "current-run";
  const recorded = { id: runId, projectId, checkoutId: exampleCheckout.checkoutId, turnId: "turn-current", status: "succeeded" };
  const openActivity = vi.fn();
  vi.mocked(invoke).mockImplementation(async (command, payload) => {
    switch (command) {
      case "inspect_conversation_run": return [recorded, conversationId];
      case "conversation_events": {
        const args = payload as { conversationId: string; runId: string; after: string };
        expect(args.conversationId).toBe(conversationId);
        const child = { itemId: "child", turnId: "turn", sessionId: "session", parentItemId: "parent" };
        const parent = { ...child, itemId: "text", parentItemId: null };
        return args.runId === runId ? { cursor: "30", more: false, events: [
          { data: { type: "assistant_final", text: "Current exact response" }, location: parent },
        ] } : args.after === "0" ? { cursor: "17", more: true, events: [
          { data: { type: "assistant_delta", text: "draft" }, location: parent },
        ] } : { cursor: "20", more: false, events: [
          { data: { type: "assistant_final", text: "Previous exact response" }, location: parent },
          { data: { type: "assistant_final", text: "Child result" }, location: child },
        ] };
      }
      case "related_saved_attempts": return [
        { runId, checkoutId: recorded.checkoutId, ordinal: 5, status: "succeeded", createdAt: 500 },
        { runId: "prior-exact", checkoutId: "prior-checkout", ordinal: 4, status: "failed", createdAt: 400 },
      ];
      case "saved_conversation_questions": return [];
      case "run_orchestration": case "conversation_checklist_progress": case "downstream_wait": return null;
      default: throw new Error(command);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><RunInspector checkout={exampleCheckout} runId={runId} conversationId={conversationId}
 onClose={vi.fn()} openWorkspace={vi.fn()} openChanges={vi.fn()} openActivity={openActivity} /></QueryClientProvider>);
  const user = userEvent.setup();
  const activity = within(await screen.findByRole("complementary", { name: "Conversation activity" }));
  await user.click(activity.getByRole("tab", { name: "Artifacts" }));
  await activity.findByText("Current exact response");
  expect(invoke).not.toHaveBeenCalledWith("related_saved_attempts", expect.anything());
  fireEvent.contextMenu(activity.getByRole("heading", { name: "Recorded output" }));
  await user.click(screen.getByRole("menuitem", { name: "Compare saved attempt output…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Compare saved attempt output" }));
  expect((await dialog.findByLabelText("Original evidence")).textContent).toBe("Previous exact response");
  expect(dialog.getByLabelText("Modified evidence").textContent).toBe("Current exact response");
  expect(dialog.getByText("prior-checkout")).toBeTruthy();
  expect(dialog.getByText("20")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("related_saved_attempts", { projectId, conversationId, runId });
  expect(invoke).toHaveBeenCalledWith("conversation_events", { conversationId, runId: "prior-exact", after: "17" });
  await user.click(dialog.getByRole("button", { name: "Open base attempt" }));
  expect(openActivity).toHaveBeenCalledExactlyOnceWith("prior-exact", conversationId);
});
