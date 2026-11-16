import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import type { DeliveryActions, DeliveryTarget } from "@/repository-delivery-model";

type AutomaticRule = {
  mode: "automatic";
  taskId: string;
  sourceCheckoutId: string;
  maximumUnits: number;
  ifUnmet: "wait" | "human";
};
type ApprovalSettings = { mode: "human" } | AutomaticRule;
type ApprovalOutcome = "human_required" | "scope_does_not_match" | "unit_limit_exceeded" |
  "another_delivery_owns_task" | "conditions_met";
type ApprovalPreview = {
  revision: string | null;
  settings: ApprovalSettings;
  facts: { taskId: string; sourceCheckoutId: string; planRevision: string; unitCount: number };
  outcome: ApprovalOutcome;
  allowsHuman: boolean;
  decisionArtifactId: string | null;
};

function status(preview: ApprovalPreview) {
  switch (preview.outcome) {
    case "human_required": return "Waiting for your approval of the selected plan and task.";
    case "scope_does_not_match": return "The rule covers a different task or source checkout.";
    case "unit_limit_exceeded": return `The reviewed plan has ${preview.facts.unitCount} logical units. The rule permits ${preview.settings.mode === "automatic" ? preview.settings.maximumUnits : 0}.`;
    case "another_delivery_owns_task": return "Another approval stage or delivery currently owns this task.";
    case "conditions_met": return `The reviewed plan matches the approved scope and has ${preview.facts.unitCount} logical units within the rule’s limit.`;
  }
}

function RuleForm({ preview, onSave, busy }: {
  preview: ApprovalPreview; onSave: (settings: ApprovalSettings) => void; busy: boolean;
}) {
  const [mode, setMode] = useState(preview.settings.mode);
  const [limit, setLimit] = useState(String(preview.settings.mode === "automatic" ? preview.settings.maximumUnits : preview.facts.unitCount));
  const [ifUnmet, setIfUnmet] = useState<AutomaticRule["ifUnmet"]>(preview.settings.mode === "automatic" ? preview.settings.ifUnmet : "wait");
  const count = Number(limit);
  const valid = Number.isInteger(count) && count > 0 && count <= 4_294_967_295;
  return <details className="pipeline-evidence">
    <summary>Implementation approval settings</summary>
    <form className="pipeline-stack p-2" onSubmit={(event) => {
      event.preventDefault();
      if (mode === "human") onSave({ mode });
      if (mode === "automatic" && valid) onSave({ mode, maximumUnits: count, ifUnmet,
        taskId: preview.facts.taskId, sourceCheckoutId: preview.facts.sourceCheckoutId });
    }}>
      <label>Decision mode <select value={mode} onChange={(event) => setMode(event.currentTarget.value === "automatic" ? "automatic" : "human")}>
        <option value="human">Human approval</option>
        <option value="automatic">Automatic approval</option>
      </select></label>
      {mode === "automatic" && <>
        <dl className="pipeline-stack break-all">
          <div><dt>Linked work</dt><dd>{preview.facts.taskId}</dd></div>
          <div><dt>Planning checkout</dt><dd>{preview.facts.sourceCheckoutId}</dd></div>
          <div><dt>Reviewed plan</dt><dd>{preview.facts.planRevision}</dd></div>
        </dl>
        <label>Maximum logical units <input type="number" min={1} step={1} value={limit} onChange={(event) => setLimit(event.currentTarget.value)} required /></label>
        <label>When conditions are unmet <select value={ifUnmet}
          onChange={(event) => setIfUnmet(event.currentTarget.value === "human" ? "human" : "wait")}>
          <option value="wait">Keep waiting</option>
          <option value="human">Ask for human approval</option>
        </select></label>
      </>}
      <Button type="submit" variant="outline" disabled={busy || (mode === "automatic" && !valid)}>Save approval settings</Button>
    </form>
  </details>;
}

export function WorkflowApproval({ scope, nodeId, planId, taskArtifactId, source, actions }: {
  scope: DeliveryTarget; nodeId: string; planId: string; taskArtifactId: string;
  source?: "operator" | "automatic_rule"; actions: DeliveryActions;
}) {
  const client = useQueryClient();
  const submission = { ...scope, nodeId, inputs: { plan: planId, task: taskArtifactId } };
  const queryKey = ["repository-delivery", scope.projectId, scope.runId, "approval", nodeId, planId, taskArtifactId];
  const preview = useQuery({ queryKey,
    queryFn: () => invoke<ApprovalPreview>("preview_workflow_approval", { submission }),
  });
  const data = preview.data;
  const refreshRule = (command: string, args: object) => actions.execute(async () => {
    const updated = await invoke<ApprovalPreview>(command, { submission, ...args });
    client.setQueryData(queryKey, updated);
  });

  return <section className="pipeline-stack" aria-label="Implementation approval">
    {source && <p role="status">{source === "automatic_rule" ? "Automatic implementation approval recorded." : "Human implementation approval recorded."}</p>}
    {preview.isPending && <p role="status">Checking implementation approval.</p>}
    {preview.error && <p role="alert">{String(preview.error)}</p>}
    {data && <>
      {!data.decisionArtifactId && <p role="status">{status(data)}</p>}
      {data.revision && <small className="break-all">Approval settings revision {data.revision}</small>}
      {!data.decisionArtifactId && data.allowsHuman && <Button disabled={actions.busy}
        onClick={() => actions.execute(() => invoke("approve_workflow_implementation", { submission }))}>Approve implementation</Button>}
      {!data.decisionArtifactId && data.revision && data.outcome === "conditions_met" && <Button variant="outline" disabled={actions.busy}
        onClick={() => refreshRule("evaluate_workflow_approval", { expectedRevision: data.revision })}>Evaluate automatic approval</Button>}
      <RuleForm key={data.revision ?? "default"} preview={data} busy={actions.busy}
        onSave={(settings) => refreshRule("set_workflow_approval_rule", { expectedRevision: data.revision, settings })} />
    </>}
  </section>;
}
