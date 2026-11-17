import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useId, useState, type FormEvent, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import type { StageGoalEvidence } from "@/workflow-stage-goal";

type CriterionStatus = "pending" | "failed" | "passed";
type VerificationStatus = "verifying" | "failed" | "completed";
type CriterionEvidence =
  | { kind: "candidate_gate"; verificationId: string; gateIndex: number }
  | { kind: "candidate_acceptance"; acceptanceId: string }
  | { kind: "operator_review"; approved: boolean; note: string | null };
type Criterion = {
  ordinal: number;
  text: string;
  status: CriterionStatus;
  evidence: CriterionEvidence | null;
};
type Verification = {
  revisionId: string;
  runId: string;
  contextId: string;
  providerResult: string | null;
  candidateId: string | null;
  status: VerificationStatus;
  criteria: Criterion[];
};
type EvidenceChoice = {
  label: string;
  candidateId: string;
  status: CriterionStatus;
  evidence: CriterionEvidence;
};

const statusLabels: Record<VerificationStatus, string> = {
  verifying: "Verification in progress",
  failed: "Verification failed",
  completed: "Verified and complete",
};
const criterionLabels: Record<CriterionStatus, string> = {
  pending: "Pending", failed: "Failed", passed: "Passed",
};
const selectClass = "w-full min-w-0 rounded-md border border-input bg-background px-2 py-1.5 text-xs";

function evidenceLabel(evidence: CriterionEvidence, gateLabel?: string) {
  switch (evidence.kind) {
    case "candidate_gate": return `${gateLabel ?? "Test"} · ${evidence.verificationId} · Gate ${evidence.gateIndex + 1}`;
    case "candidate_acceptance": return `Candidate acceptance ${evidence.acceptanceId}`;
    case "operator_review": return evidence.approved ? "Approved by operator review" : "Rejected by operator review";
  }
}

function CriterionEvidenceForm({ criterion, choices, projectId, runId, onSaved, allowOperatorReview }: {
  criterion: Criterion;
  choices: EvidenceChoice[];
  projectId: string;
  runId: string;
  onSaved: (verification: Verification) => void;
  allowOperatorReview: boolean;
}) {
  const choiceLabel = useId();
  const noteLabel = useId();
  const [selection, setSelection] = useState("");
  const [note, setNote] = useState("");
  const review = selection === "approve" || selection === "reject";
  const evidence: CriterionEvidence | undefined = review
    ? { kind: "operator_review", approved: selection === "approve", note: note.trim() || null }
    : choices.find((choice) => JSON.stringify(choice.evidence) === selection)?.evidence;
  const save = useMutation({
    mutationFn: (evidence: CriterionEvidence) => invoke<Verification>("record_goal_criterion_evidence", {
      projectId, runId, ordinal: criterion.ordinal, evidence,
    }),
    onSuccess: (updated) => {
      setSelection("");
      setNote("");
      onSaved(updated);
    },
  });
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (selection && evidence && !save.isPending) save.mutate(evidence);
  };
  return <form onSubmit={submit} className="grid gap-1.5">
    <label htmlFor={choiceLabel}>Evidence for criterion {criterion.ordinal}</label>
    <select className={selectClass} id={choiceLabel} value={selection}
      onChange={(event) => setSelection(event.currentTarget.value)}>
      <option value="">Choose evidence</option>
      {choices.length > 0 && <optgroup label="Recorded candidate evidence">
        {choices.map((choice) => <option key={JSON.stringify(choice.evidence)} value={JSON.stringify(choice.evidence)}>
          {evidenceLabel(choice.evidence, choice.label)} · {criterionLabels[choice.status]} · Candidate {choice.candidateId}
        </option>)}
      </optgroup>}
      {allowOperatorReview && <optgroup label="Operator review of this result">
        <option value="approve">Approve criterion</option>
        <option value="reject">Reject criterion</option>
      </optgroup>}
    </select>
    {review && <>
      <label htmlFor={noteLabel}>Review note (optional)</label>
      <textarea id={noteLabel} className={selectClass} value={note} maxLength={2048}
        onChange={(event) => setNote(event.currentTarget.value)} />
    </>}
    <Button type="submit" variant="outline" className="justify-self-start"
      disabled={!selection || !evidence || save.isPending}>Record evidence</Button>
    {save.error && <p role="alert">{String(save.error)}</p>}
  </form>;
}

export function GoalVerificationPanel({ projectId, runId, live = false, requiredEvidence, candidateId, actions, onVerificationSaved }: {
  projectId: string;
  runId: string;
  live?: boolean;
  requiredEvidence?: readonly StageGoalEvidence[];
  candidateId?: string;
  actions?: (result: Verification) => ReactNode;
  onVerificationSaved?: () => void;
}) {
  const client = useQueryClient();
  const key = ["goal-verification", projectId, runId, live];
  const choicesKey = ["goal-verification", projectId, runId, "choices"];
  const verification = useQuery({
    queryKey: key,
    queryFn: () => invoke<Verification | null>("goal_verification", { projectId, runId }),
    refetchInterval: (query) => live || query.state.data?.criteria.some(({ status }) => status === "pending") ? 800 : false,
  });
  const result = verification.data;
  const choices = useQuery({
    queryKey: choicesKey,
    queryFn: () => invoke<EvidenceChoice[]>("list_goal_criterion_evidence", { projectId, runId }),
    enabled: Boolean(result && result.criteria.length),
    refetchInterval: result && result.status !== "completed" ? 1000 : false,
  });
  if (verification.error) return <p role="alert" className="text-xs">{String(verification.error)}</p>;
  if (!result) return null;
  const editable = result.status !== "completed";
  const onSaved = (updated: Verification) => {
    client.setQueryData<Verification>(key, (previous) => previous?.status === "completed" ? previous : updated);
    void client.invalidateQueries({ queryKey: key });
    void client.invalidateQueries({ queryKey: choicesKey });
    void client.invalidateQueries({ queryKey: ["run-orchestration", projectId, runId] });
    onVerificationSaved?.();
  };
  return <section aria-label="Goal verification" className="grid gap-2 text-xs">
    <h3 className="text-sm font-medium">Goal verification</h3>
    <p role="status">{statusLabels[result.status]}</p>
    {result.providerResult && <p>Provider verdict {result.providerResult.replaceAll("_", " ")}</p>}
    {result.candidateId && <p className="break-all">Candidate {result.candidateId}</p>}
    {choices.error && <p role="alert">{String(choices.error)}</p>}
    {result.criteria.length > 0 && <ol aria-label="Required goal criteria" className="grid gap-2">
      {result.criteria.map((criterion) => {
        const required = requiredEvidence?.[criterion.ordinal - 1];
        const qualified = (choices.data ?? []).filter((choice) =>
          (!required || choice.evidence.kind === required) && (!candidateId || candidateId === choice.candidateId));
        return <li key={criterion.ordinal} className="grid gap-1 border-t border-border pt-2">
          <p className="font-medium">{criterion.text}</p>
          <p>{criterionLabels[criterion.status]}</p>
          {criterion.evidence && <p className="break-words text-muted-foreground">{evidenceLabel(criterion.evidence,
            choices.data?.find((item) => JSON.stringify(item.evidence) === JSON.stringify(criterion.evidence))?.label)}</p>}
          {criterion.evidence?.kind === "operator_review" && criterion.evidence.note && <p>{criterion.evidence.note}</p>}
          {editable && <CriterionEvidenceForm criterion={criterion} choices={qualified} allowOperatorReview={required !== "candidate_gate"}
            projectId={projectId} runId={runId} onSaved={onSaved} />}
        </li>;
      })}
    </ol>}
    {actions?.(result)}
  </section>;
}
