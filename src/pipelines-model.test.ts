import { describe, expect, it } from "vitest";
import { advancePipelineRun, beginPipelineRun, getSampleModel, stageKinds, declineStage, recordPipelineGit, resolveStage, retryStage, runStatus, stageAttempt, validateWorkflow } from "./pipelines-model";
import type { PipelineRun, Role, Stage, StageKind, StageNode, WorkflowRevision } from "./pipelines-model";

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
    run = settle(resolveStage(run, "accept"));
    expect(stageAttempt(run, "commit")?.detail).toBe("Waiting for commit");
    expect(resolveStage(run, "commit")).toBe(run);
    const commit = { branch: "agent/release-checklist", sha: "d73b42a91be3c8c63cf128379192f229061cca70e" };
    let withMerge = settle(recordPipelineGit({ ...run, workflow: { ...run.workflow, mergeRequired: true } }, { commit }));
    expect(stageAttempt(withMerge, "merge")?.detail).toBe("Waiting for merge");
    expect(resolveStage(withMerge, "merge")).toBe(withMerge);
    const merge = { branch: "main", sha: "13a63c87cbb034dbf6a6d4deda4f93facbe28daae" };
    withMerge = settle(recordPipelineGit(withMerge, { merge }));
    expect(stageAttempt(withMerge, "merge")?.git).toEqual(merge);
    expect(runStatus(withMerge)).toBe("succeeded");
    run = settle(recordPipelineGit(run, { commit }));
    expect(stageAttempt(run, "commit")?.git).toEqual(commit);
    expect(stageAttempt(run, "merge")?.status).toBe("skipped");
    expect(runStatus(run)).toBe("succeeded");
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

function createStage(kind: StageKind, id: string, x: number, y: number): StageNode {
  const stageRoles: Partial<Record<StageKind, string>> = { plan: "planner", review: "reviewer", implementation: "implementer", verify: "reviewer" };
  const outputs: Record<StageKind, Stage["output"]> = {
    plan: "plan", condition: "decision", review: "review", join: "review", task: "task",
    approval: "release", implementation: "patch", verify: "checks", accept: "decision", commit: "commit", merge: "integration",
  };
  return {
    id, type: "stage", position: { x, y }, ariaLabel: stageKinds[kind],
    data: {
      title: stageKinds[kind], kind, roleId: stageRoles[kind] ?? "", mode: ["implementation", "commit", "merge"].includes(kind) ? "write" : kind === "plan" ? "plan" : "read-only",
      model: null, timeoutMinutes: 10, retryLimit: kind === "verify" ? 1 : 0, output: outputs[kind], requiredCapability: "",
      subagents: 0, release: "human", predicate: "reviews-passed", goal: "", evidence: "", goalMinutes: 15, goalContinuation: "managed",
    },
  };
}

function createDeliveryWorkflow(): WorkflowRevision {
  const nodes: StageNode[] = [
    createStage("plan", "plan", 24, 160),
    createStage("condition", "condition", 224, 160),
    { ...createStage("review", "review", 424, 42), data: { ...createStage("review", "review", 424, 42).data, title: "Cross-review" } },
    { ...createStage("review", "migration", 424, 276), data: { ...createStage("review", "migration", 424, 276).data, title: "Migration review" } },
    createStage("join", "join", 624, 160),
    createStage("task", "task", 824, 160),
    createStage("approval", "approval", 824, 360),
    createStage("implementation", "implementation", 624, 360),
    createStage("verify", "verify", 424, 360),
    createStage("accept", "accept", 224, 360),
    createStage("commit", "commit", 24, 360),
    createStage("merge", "merge", 24, 540),
  ];
  const pairs: [string, string][] = [
    ["plan", "condition"], ["condition", "review"], ["condition", "migration"], ["review", "join"],
    ["migration", "join"], ["join", "task"], ["task", "approval"], ["approval", "implementation"],
    ["implementation", "verify"], ["verify", "accept"], ["accept", "commit"], ["commit", "merge"],
  ];
  const roles = [
    { id: "planner", title: "Planner", model: getSampleModel("gpt-6.1-sol")?.selection ?? null, effort: "high", instructions: "Read the pinned release note and propose a scoped plan.", tools: "Repository read", completion: "Return a versioned plan and acceptance criteria.", review: "Cross-review", requirements: ["Interactive questions"] },
    { id: "reviewer", title: "Reviewer", model: getSampleModel("claude-sonnet-5-5")?.selection ?? null, effort: "medium", instructions: "Check the candidate against the release criteria.", tools: "Repository read", completion: "Return structured findings and verification results.", review: "Independent", requirements: [] },
    { id: "implementer", title: "Implementer", model: getSampleModel("gpt-6.1-terra")?.selection ?? null, effort: "high", instructions: "Implement the approved plan in the assigned worktree.", tools: "Approved worktree write", completion: "Return a patch and verification evidence.", review: "Human approval", requirements: [] },
  ] satisfies Role[];
  return {
    label: "Task delivery",
    revision: 3,
    nodes,
    edges: pairs.map(([source, target]) => ({
      id: source + "-" + target, source, target,
      label: source === "condition" ? (target === "migration" ? "If migration" : "Always") : undefined,
      data: target === "migration" ? { branch: "migration" } : undefined,
    })),
    roles,
  };
}
