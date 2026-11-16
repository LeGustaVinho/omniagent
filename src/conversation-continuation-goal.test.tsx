// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationContinuationDialog, type ConversationContinuationReview } from "@/conversation-continuation";
import { goalDefinition, type GoalTransitionScope } from "@/conversation-goal";

afterEach(cleanup);

it("shows the recorded goal and target scope before confirming a separate checkout", async () => {
  const original: GoalTransitionScope = {
    projectId: "project", checkoutId: "original", checkoutRoot: "D:/original", provider: "codex",
    profileId: "original-profile", model: "goal-model", effort: "high", mode: "read_only",
  };
  const review: ConversationContinuationReview = {
    status: "review_required", previewId: "sealed", contextId: "current", reason: "checkout_changed",
    settings: { projectDefault: "same_tab", conversationOverride: null, effective: "same_tab" },
    seed: [{ revisionId: "message", role: "assistant", text: "Prior discussion", attachments: [] }],
    goalTransition: {
      recordedGoal: {
        revisionId: "goal", definition: goalDefinition("Check release packages"), continuationOwner: "native", controlState: "paused",
        checkpoint: { ordinal: 3, runId: "run-3", nextAction: "continue" }, checkpointRunId: "run-3", stopReviewRunId: null, checkpointContextId: "saved", scope: original,
        reviewToken: "recorded-review",
      },
      nextTurnScope: { ...original, checkoutId: "target", checkoutRoot: "D:/target", profileId: "target-profile", model: "replacement-model" },
      sameTabAvailable: false,
    },
  };
  const onConfirm = vi.fn();
  render(<ConversationContinuationDialog review={review} pending={false} onConfirm={onConfirm} onClose={vi.fn()} />);
  const goal = within(screen.getByRole("region", { name: "Recorded goal" }));
  expect(goal.getByText("Check release packages")).toBeTruthy();
  expect(goal.getByText(/Checkpoint 3/)).toBeTruthy();
  expect(goal.getByText("D:/original")).toBeTruthy();
  expect(goal.getByText("goal-model")).toBeTruthy();
  const target = within(screen.getByRole("region", { name: "Next turn scope" }));
  expect(target.getByText("D:/target")).toBeTruthy();
  expect(target.getByText("replacement-model")).toBeTruthy();
  expect((screen.getByRole("radio", { name: "Continue in this tab" }) as HTMLInputElement).disabled).toBe(true);
  expect((screen.getByRole("radio", { name: "Fork with context" }) as HTMLInputElement).checked).toBe(true);
  expect(screen.getByText("Start a separate goal in the new conversation.")).toBeTruthy();
  expect(screen.getByText("Prior discussion")).toBeTruthy();
  expect(onConfirm).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("button", { name: "Continue and send" }));
  expect(onConfirm).toHaveBeenCalledWith("fork_with_context");
});
