import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { deliveryGitModes, deliveryGitSetting, simulatedDeliveryGitModes, type DeliveryGitPolicy, type SimulatedDeliveryGitPolicy } from "@/delivery-git-policy";
import { useGitSimulation } from "@/git-simulation";
import { SettingRow } from "@/settings-controls";

function PolicySelect<Policy extends SimulatedDeliveryGitPolicy>({ modes, value, pending, onChange }: {
  modes: readonly (readonly [Policy, string])[];
  value?: Policy;
  pending?: boolean;
  onChange: (policy: Policy) => void;
}) {
  const { id, label, description } = deliveryGitSetting;
  return <SettingRow id={id} label={label} description={description}>
    <select id={id} aria-describedby={`${id}-description`} className="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
      disabled={value === undefined || pending} value={value ?? ""} onChange={(event) => onChange(event.target.value as Policy)}>
      {value === undefined && <option value="">{pending ? "Loading policy…" : "Policy unavailable"}</option>}
      {modes.map(([policy, label]) => <option key={policy} value={policy}>{label}</option>)}
    </select>
  </SettingRow>;
}

export function SimulatedDeliveryGitSettings() {
  const git = useGitSimulation();
  return <PolicySelect modes={simulatedDeliveryGitModes} value={git.gitMode} onChange={git.setGitMode} />;
}

export function DeliveryGitSettings({ projectId }: { projectId: string }) {
  const client = useQueryClient();
  const queryKey = ["delivery-git-policy", projectId];
  const policy = useQuery({
    queryKey,
    queryFn: () => invoke<DeliveryGitPolicy>("delivery_git_policy", { projectId }),
  });
  const save = useMutation({
    mutationFn: (policy: DeliveryGitPolicy) => invoke("set_delivery_git_policy", { projectId, policy }),
    onSuccess: (_, saved) => client.setQueryData(queryKey, saved),
  });

  return <>
    <PolicySelect modes={deliveryGitModes} value={policy.data} pending={policy.isPending || save.isPending} onChange={save.mutate} />
    {!!(policy.error || save.error) && <p role="alert" className="settings-result settings-result-error">
      {String(save.error || policy.error)}
    </p>}
    {save.isSuccess && <p role="status" className="sr-only">Git policy saved</p>}
  </>;
}
