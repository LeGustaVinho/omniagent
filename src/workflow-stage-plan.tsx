import { useId } from "react";
import type { SessionMode } from "@/bindings/SessionMode";

const MAX_SUBAGENTS = 32;

export type StagePlanConfiguration = {
  questionRoute: "native" | "structured_turn";
  maxSubagents: number;
};

export function stagePlanMode(plan: StagePlanConfiguration | undefined): SessionMode {
  return plan?.questionRoute === "structured_turn" ? "read_only" : "plan";
}

export function WorkflowStagePlan({ plan, disabled, onChange }: {
  plan?: StagePlanConfiguration;
  disabled: boolean;
  onChange: (plan: StagePlanConfiguration | undefined) => void;
}) {
  const hint = useId();

  return <fieldset disabled={disabled} aria-describedby={hint}>
    <legend>Plan Mode</legend>
    <label>Planning questions<select value={plan?.questionRoute ?? "default"} onChange={(event) => {
      const route = event.currentTarget.value;
      if (route === "native" || route === "structured_turn") {
        onChange({ questionRoute: route, maxSubagents: plan?.maxSubagents ?? 0 });
      } else onChange(undefined);
    }}>
      <option value="default">Use workflow defaults</option>
      <option value="native">Require verified CLI planning questions</option>
      <option value="structured_turn">Use app-managed questions in a read-only turn</option>
    </select></label>
    {plan && <label>Maximum subagents<input type="number" min={0} max={MAX_SUBAGENTS} step={1}
      value={plan.maxSubagents} onChange={(event) => {
        const value = event.currentTarget.valueAsNumber;
        if (Number.isInteger(value) && value >= 0 && value <= MAX_SUBAGENTS) {
          onChange({ ...plan, maxSubagents: value });
        }
      }} /></label>}
    <p className="text-sm text-muted-foreground" id={hint}>The planner proposes a plan for approval. The limit caps app-managed subagents. Under Strict total, it also caps qualified native subagents.</p>
  </fieldset>;
}
