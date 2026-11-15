import { describe, expect, it } from "vitest";
import { advancePipelineRun, beginPipelineRun, createDeliveryWorkflow, declineStage, resolveStage, retryStage, runStatus, stageAttempt, validateWorkflow } from "./pipelines-model";
import type { PipelineRun } from "./pipelines-model";

function settle(run: PipelineRun) {
  let current = run;
  for (let access = 0; access < 30; access += 1) {
    const next = advancePipelineRun(current);
    if (next === current) return current;
    current = next;
  }
  throw new Error("The run did not reach a waiting or finished state.");
}

describe("pipeline scenario", () => {
  it("rejects cycles and any writer path which bypasses the release", () => {
    const workflow = createDeliveryWorkflow();
    expect(validateWorkflow(workflow)).toEqual([]);
    const bypass = { id: "bypass", source: "plan", target: "implementation" };
    expect(validateWorkflow({ ...workflow, edges: [...workflow.edges, bypass] })).toContain("Implementation must follow Approve implementation on every path.");
    const cycle = { id: "cycle", source: "verify", target: "plan" };
    expect(validateWorkflow({ ...workflow, edges: [...workflow.edges, cycle] })).toContain("The graph contains a cycle. Use a stage's bounded retry limit instead.");
  });

  it("skips the excluded branch, pins the revision and preserves failed attempts after retry", () => {
    const workflow = createDeliveryWorkflow();
    const addedToHistory = beginPipelineRun(1, workflow);
    expect(addedToHistory.workflow.revision).toBe(3);
    let run = settle(addedToHistory);
    expect(stageAttempt(run, "plan")?.status).toBe("waiting");
    run = settle(resolveStage(run, "plan", false));
    expect(stageAttempt(run, "migration")?.status).toBe("skipped");
    expect(stageAttempt(run, "join")?.status).toBe("succeeded");
    expect(stageAttempt(run, "approval")?.status).toBe("waiting");
    expect(stageAttempt(run, "implementation")?.status).toBe("queued");

    run = settle(resolveStage(run, "approval"));
    expect(stageAttempt(run, "verify")?.status).toBe("failed");
    run = settle(retryStage(run, "verify"));
    expect(run.attempts.verify?.map((attempt) => attempt.status)).toEqual(["failed", "succeeded"]);
    expect(stageAttempt(run, "accept")?.status).toBe("waiting");
    expect(runStatus(settle(resolveStage(run, "accept")))).toBe("succeeded");
  });

  it("runs both selected branches and keeps repository writes waiting after rejection", () => {
    const initial = settle(beginPipelineRun(1, createDeliveryWorkflow()));
    const plan = settle(resolveStage(initial, "plan", true));
    expect(stageAttempt(plan, "migration")?.status).toBe("succeeded");
    expect(stageAttempt(plan, "review")?.status).toBe("succeeded");
    const declined = settle(declineStage(plan, "approval"));
    expect(stageAttempt(declined, "approval")?.status).toBe("failed");
    expect(stageAttempt(declined, "implementation")?.status).toBe("queued");
    expect(retryStage(declined, "approval")).toBe(declined);
  });
});
