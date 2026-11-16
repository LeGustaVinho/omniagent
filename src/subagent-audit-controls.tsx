import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ConversationSubagentAudit } from "@/bindings/ConversationSubagentAudit";
import type { SubagentAuditField } from "@/bindings/SubagentAuditField";
import type { SubagentAuditGap } from "@/bindings/SubagentAuditGap";
import type { SubagentAuditPolicy } from "@/bindings/SubagentAuditPolicy";
import { SettingRow } from "@/settings-controls";

const policyLabels: Record<SubagentAuditPolicy, string> = {
  available: "Show available evidence",
  complete: "Require complete audit evidence",
};

const fieldLabels: Record<SubagentAuditField, string> = {
  identity: "Identity",
  prompt: "Prompt",
  context_provenance: "Context provenance",
  response: "Response",
};
const completeFields: SubagentAuditField[] = ["identity", "prompt", "context_provenance", "response"];

type AuditPolicySource = "project_default" | "conversation_override" | "workflow" | "originating_attempt";
type ManagedReason = "native_evidence_unavailable" | "zero_native_allowance";
type AuditRoute = { kind: "available" }
  | { kind: "managed"; reason: ManagedReason | null }
  | { kind: "native"; evidenceId: string };

type SavedAttemptAudit = {
  effectivePolicy: SubagentAuditPolicy;
  policySource: AuditPolicySource | null;
  policyConversationId: string;
  requiredFields: SubagentAuditField[];
  route: AuditRoute;
};

const policySourceLabels: Record<AuditPolicySource, string> = {
  project_default: "Inherited from the project",
  conversation_override: "Conversation override",
  workflow: "Required by the workflow stage",
  originating_attempt: "Inherited from the originating attempt",
};

const managedReasons: Record<ManagedReason, string> = {
  native_evidence_unavailable: "The selected CLI scope lacked a current native audit qualification.",
  zero_native_allowance: "This attempt was allocated zero native children.",
};

function auditRouteLabel(route: AuditRoute) {
  switch (route.kind) {
    case "available": return "No audit-specific route required.";
    case "managed":
      return `Managed delegation required. ${route.reason ? managedReasons[route.reason] : "The reason was not recorded for this attempt."}`;
    case "native": return "Qualified native delegation.";
  }
}

function RequiredAuditFields({ fields }: { fields: SubagentAuditField[] }) {
  return <>
    <dt>Required fields</dt>
    <dd>{fields.length ? fields.map((field) => fieldLabels[field]).join(", ") : "None"}</dd>
  </>;
}

function auditOptions() {
  return Object.entries(policyLabels).map(([value, label]) => <option key={value} value={value}>{label}</option>);
}

export function ProjectSubagentAuditControl({ projectId }: { projectId: string }) {
  const client = useQueryClient();
  const key = ["project-subagent-audit", projectId];
  const policy = useQuery({
    queryKey: key,
    queryFn: () => invoke<SubagentAuditPolicy>("project_subagent_audit", { projectId }),
  });
  const save = useMutation({
    mutationFn: (policy: SubagentAuditPolicy) => invoke("set_project_subagent_audit", { projectId, policy }),
    onSuccess: (_, value) => {
      client.setQueryData(key, value);
      return Promise.all([
        client.invalidateQueries({ queryKey: ["conversation-subagent-audit"] }),
        client.invalidateQueries({ queryKey: ["project-job-queue", projectId] }),
      ]);
    },
  });
  return <>
    <SettingRow id="subagent-audit" label="Default subagent audit policy"
      description="Complete evidence requires each child's identity, prompt, context provenance and response.">
      <select id="subagent-audit" aria-describedby="subagent-audit-description" disabled={!policy.data || save.isPending}
        value={save.isPending ? save.variables : policy.data ?? ""}
        onChange={(event) => save.mutate(event.currentTarget.value as SubagentAuditPolicy)}>
        {!policy.data && <option value="">Loading policy…</option>}
        {auditOptions()}
      </select>
    </SettingRow>
    {(policy.error || save.error) && <p className="settings-result settings-result-error" role="alert">{String(policy.error || save.error)}</p>}
  </>;
}

export function ConversationSubagentAuditControl({ conversationId, open }: { conversationId: string; open: boolean }) {
  const client = useQueryClient();
  const key = ["conversation-subagent-audit", conversationId];
  const policy = useQuery({
    queryKey: key,
    queryFn: () => invoke<ConversationSubagentAudit>("conversation_subagent_audit", { conversationId }),
    enabled: open,
  });
  const save = useMutation({
    mutationFn: (policy: SubagentAuditPolicy | null) =>
      invoke<ConversationSubagentAudit>("set_conversation_subagent_audit", { conversationId, policy }),
    onSuccess: (settings) => {
      client.setQueryData(key, settings);
      return client.invalidateQueries({ queryKey: ["project-job-queue"] });
    },
  });
  const effective = policy.data?.conversationOverride ?? policy.data?.projectDefault;
  return <>
    <label>
      Subagent audit policy
      <select disabled={!policy.data || save.isPending}
        value={(save.isPending ? save.variables : policy.data?.conversationOverride) ?? ""}
        onChange={(event) => save.mutate(event.currentTarget.value ? event.currentTarget.value as SubagentAuditPolicy : null)}>
        <option value="">{policy.data ? `Use project default · ${policyLabels[policy.data.projectDefault]}` : "Loading policy…"}</option>
        {auditOptions()}
      </select>
    </label>
    {effective && <dl className="grid gap-1 text-xs text-muted-foreground">
      <dt>Effective audit policy</dt>
      <dd>{policyLabels[effective]} · {policySourceLabels[policy.data?.conversationOverride ? "conversation_override" : "project_default"]}</dd>
      <RequiredAuditFields fields={effective === "complete" ? completeFields : []} />
    </dl>}
    {(policy.error || save.error) && <p className="conv-alert" role="alert">{String(policy.error || save.error)}</p>}
  </>;
}

export function AttemptSubagentAudit({ conversationId, runId, live, gaps }: {
  conversationId: string;
  runId: string;
  live: boolean;
  gaps: SubagentAuditGap[];
}) {
  const audit = useQuery({
    queryKey: ["conversation-run", runId, "subagent-audit", conversationId],
    queryFn: () => invoke<SavedAttemptAudit | null>("attempt_subagent_audit", { conversationId, runId }),
    staleTime: Infinity,
  });
  const evidence = audit.data;
  const source = evidence?.policySource === "conversation_override" && evidence.policyConversationId !== conversationId
    ? "Delegating conversation override"
    : evidence?.policySource ? policySourceLabels[evidence.policySource] : "Policy source unavailable";
  return <section aria-label="Subagent audit" className="grid gap-2">
    <h2 className="text-sm font-medium">Subagent audit</h2>
    {audit.isPending && <p className="text-xs text-muted-foreground">Loading saved audit policy…</p>}
    {audit.error && <p role="alert" className="text-xs text-muted-foreground">{String(audit.error)}</p>}
    {evidence === null && <p className="text-xs text-muted-foreground">Attempt audit unavailable.</p>}
    {evidence && <dl className="grid gap-1 text-xs text-muted-foreground">
      <dt>Effective policy at admission</dt><dd>{policyLabels[evidence.effectivePolicy]}</dd>
      <dt>Policy source</dt><dd>{source}</dd>
      <RequiredAuditFields fields={evidence.requiredFields} />
      <dt>Delegation route</dt><dd>{auditRouteLabel(evidence.route)}</dd>
      {evidence.route.kind === "native" && <><dt>Qualification evidence</dt><dd className="break-all">{evidence.route.evidenceId}</dd></>}
    </dl>}
    {gaps.length ? <ol aria-label="Observed audit gaps" className="grid gap-1 text-xs text-muted-foreground">
      {gaps.map((gap) => <li key={gap.source}>
        {gap.nativeDelegationDetected && <p role="alert">The CLI reported a native child after native delegation was disabled.</p>}
        {gap.missing.length > 0 && <p>
          {gap.source === "native" ? "Native delegation" : "Managed child"} · Missing {gap.missing.map((field) => fieldLabels[field]).join(", ")}
          {gap.source === "native" && ` · ${gap.affectedChildren} identified with gaps · ${gap.unidentified} without confirmed identity`}
        </p>}
      </li>)}
    </ol> : live && <p className="text-xs text-muted-foreground">Audit gaps are recorded when the attempt ends.</p>}
  </section>;
}
