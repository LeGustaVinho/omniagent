import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ConversationSubagentAudit } from "@/bindings/ConversationSubagentAudit";
import type { SubagentAuditPolicy } from "@/bindings/SubagentAuditPolicy";
import { SettingRow } from "@/settings-controls";

const policyLabels: Record<SubagentAuditPolicy, string> = {
  available: "Show available evidence",
  complete: "Require complete audit evidence",
};

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
    {(policy.error || save.error) && <p className="conv-alert" role="alert">{String(policy.error || save.error)}</p>}
  </>;
}
