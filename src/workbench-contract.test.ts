import { expect, it } from "vitest";
import { nextDeliveryStep, scenarios, type DeliveryAction, type DeliveryStep } from "@/workbench-contract";

it("runs the delivery scenario only in the order the review gates allow", () => {
  let step: DeliveryStep = scenarios.delivery.initialStep;
  expect(nextDeliveryStep(step, "approve")).toBe("note");
  const flow: [DeliveryAction, DeliveryStep][] = [
    ["start", "question"], ["answer", "plan"], ["approve", "queued"],
    ["dispatch", "running"], ["finish", "review"], ["verify", "verified"],
    ["accept", "accepted"], ["merge", "merged"],
  ];
  for (const [action, expected] of flow) {
    step = nextDeliveryStep(step, action);
    expect(step).toBe(expected);
  }
});

it("returns an invalidated candidate to review and resumes an interrupted attempt through the queue", () => {
  const invalidated = nextDeliveryStep("accepted", "invalidate");
  expect(invalidated).toBe("review");
  expect(nextDeliveryStep(invalidated, "merge")).toBe("review");
  const recovered = nextDeliveryStep(scenarios.recovery.initialStep, "resume");
  expect(recovered).toBe("queued");
  expect(nextDeliveryStep(recovered, "dispatch")).toBe("running");
});
