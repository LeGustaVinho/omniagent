import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { deliveryGitModes, deliveryGitSetting, deliveryMilestoneKey, type DeliveryGitPolicy,
  type DeliveryMilestonePolicy, type DestinationDriftPolicy } from "@/delivery-git-policy";
import { useGitSimulation } from "@/git-simulation";
import { SettingRow } from "@/settings-controls";

function PolicySelect({ value, pending, onChange }: {
  value?: DeliveryGitPolicy;
  pending?: boolean;
  onChange: (policy: DeliveryGitPolicy) => void;
}) {
  const { id, label, description } = deliveryGitSetting;
  return <SettingRow id={id} label={label} description={description}>
    <select id={id} aria-describedby={`${id}-description`} className="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
      disabled={value === undefined || pending} value={value ?? ""} onChange={(event) => onChange(event.target.value as DeliveryGitPolicy)}>
      {value === undefined && <option value="">{pending ? "Loading policy…" : "Policy unavailable"}</option>}
      {deliveryGitModes.map(([policy, label]) => <option key={policy} value={policy}>{label}</option>)}
    </select>
  </SettingRow>;
}

export function SimulatedDeliveryGitSettings() {
  const git = useGitSimulation();
  return <PolicySelect value={git.gitMode} onChange={git.setGitMode} />;
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
    <PolicySelect value={policy.data} pending={policy.isPending || save.isPending} onChange={save.mutate} />
    {!!(policy.error || save.error) && <p role="alert" className="settings-result settings-result-error">
      {String(save.error || policy.error)}
    </p>}
    {save.isSuccess && <p role="status" className="sr-only">Git policy saved</p>}
    <DeliveryIntegrationSettings projectId={projectId} />
  </>;
}

function DeliveryIntegrationSettings({ projectId }: { projectId: string }) {
  const client = useQueryClient();
  const milestoneKey = deliveryMilestoneKey(projectId);
  const driftKey = ["delivery-destination-drift-policy", projectId];
  const milestone = useQuery({
    queryKey: milestoneKey,
    queryFn: () => invoke<DeliveryMilestonePolicy>("delivery_milestone_policy", { projectId }),
  });
  const drift = useQuery({
    queryKey: driftKey,
    queryFn: () => invoke<DestinationDriftPolicy>("delivery_destination_drift_policy", { projectId }),
  });
  const saveMilestone = useMutation({
    mutationFn: (policy: DeliveryMilestonePolicy) => invoke<DeliveryMilestonePolicy>("set_delivery_milestone_policy", { projectId, policy }),
    onSuccess: (saved) => client.setQueryData(milestoneKey, saved),
  });
  const saveDrift = useMutation({
    mutationFn: (policy: DestinationDriftPolicy) => invoke<DestinationDriftPolicy>("set_delivery_destination_drift_policy", { projectId, policy }),
    onSuccess: (saved) => client.setQueryData(driftKey, saved),
  });
  const error = saveMilestone.error ?? saveDrift.error ?? milestone.error ?? drift.error;
  return <>
    {milestone.data && <CompletionSettings key={JSON.stringify(milestone.data)} saved={milestone.data}
      pending={saveMilestone.isPending} onSave={saveMilestone.mutate} />}
    <SettingRow id="destination-drift" label="When the destination branch advances">
      <select id="destination-drift" className="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
        disabled={drift.data === undefined || saveDrift.isPending} value={drift.data ?? ""}
        onChange={(event) => saveDrift.mutate(event.target.value as DestinationDriftPolicy)}>
        {drift.data === undefined && <option value="">{drift.isPending ? "Loading policy…" : "Policy unavailable"}</option>}
        <option value="pause">Pause and review the update</option>
        <option value="prepare_and_recheck">Prepare a conflict-free update and rerun checks automatically</option>
      </select>
    </SettingRow>
    {error && <p role="alert" className="settings-result settings-result-error">{String(error)}</p>}
    {(saveMilestone.isSuccess || saveDrift.isSuccess) && <p role="status" className="sr-only">Integration settings saved</p>}
  </>;
}

function CompletionSettings({ saved, pending, onSave }: {
  saved: DeliveryMilestonePolicy;
  pending: boolean;
  onSave: (policy: DeliveryMilestonePolicy) => void;
}) {
  const [milestone, setMilestone] = useState(saved.milestone);
  const [branch, setBranch] = useState(saved.destinationBranch ?? "");
  const destinationBranch = branch.trim() || null;
  const changed = milestone !== saved.milestone || destinationBranch !== saved.destinationBranch;
  const canSave = changed && !pending && (milestone === "verified_commit" || destinationBranch !== null);

  return <form onSubmit={(event) => { event.preventDefault(); if (canSave) onSave({ milestone, destinationBranch }); }}>
    <SettingRow id="delivery-milestone" label="Complete repository work at">
      <select id="delivery-milestone" className="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
        value={milestone} disabled={pending} onChange={(event) => setMilestone(event.target.value as DeliveryMilestonePolicy["milestone"])}>
        <option value="verified_commit">Verified commit</option>
        <option value="verified_merge">Verified merge</option>
      </select>
    </SettingRow>
    <SettingRow id="delivery-destination" label="Merge destination" description="Select a local branch for the Verified merge milestone.">
      <input id="delivery-destination" className="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
        aria-describedby="delivery-destination-description" value={branch} required={milestone === "verified_merge"}
        disabled={pending} onChange={(event) => setBranch(event.target.value)} />
    </SettingRow>
    <Button type="submit" variant="outline" disabled={!canSave}>Save completion settings</Button>
  </form>;
}
