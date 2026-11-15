import { describe, expect, it } from "vitest";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { RunEvent } from "@/bindings/RunEvent";
import type { RunSnapshot } from "@/bindings/RunSnapshot";
import { applyRunEvent } from "@/run-projection";

const checkout: ProjectCheckout = {
  projectId: "project",
  repositoryRoot: "C:\\repos\\project",
  checkoutId: "checkout",
  checkoutRoot: "C:\\repos\\checkout",
};

describe("fixture event projection", () => {
  it("keeps full SQLite sequence precision and drops replay duplicates", () => {
    const snapshot: RunSnapshot = { cursor: "9007199254740992", runs: [], recentEvents: [] };
    const event: RunEvent = {
      seq: "9007199254740993",
      runId: "run",
      turnId: "turn",
      data: { type: "status", status: "running" },
    };
    const updated = applyRunEvent(snapshot, checkout, event);
    expect(updated.runs[0]).toEqual({
      id: "run",
      turnId: "turn",
      projectId: "project",
      checkoutId: "checkout",
      status: "running",
    });
    expect(updated.recentEvents).toEqual([event]);
    expect(applyRunEvent(updated, checkout, event)).toBe(updated);
  });
});
