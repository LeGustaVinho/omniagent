import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import { ModelPicker } from "@/components/model-picker";
import { availableModelSelection, useLiveModelOptions } from "@/live-models";
import {
  CHECKOUT_MODEL_DEFAULTS, EXECUTION_PROFILE_MODEL, PROJECT_MODEL_DEFAULTS,
  useExecutionProfileDefault, useProjectModelDefaults,
  type ModelDefaultPurpose, type ProjectModelDefaults,
} from "@/model-defaults";
import { SettingRow } from "@/settings-controls";

const providerLabels: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Antigravity" };

export const projectModelSettings = {
  conversation: { id: "conversation-model", label: "Conversation model", description: "Start new conversation turns with this model." },
  planning: { id: "planning-model", label: "Planning model", description: "Use this model for planning and as the default for context compaction." },
  implementation: { id: "implementation-model", label: "Implementation model", description: "Use this model for approved repository work that has no planned assignment." },
} satisfies Record<ModelDefaultPurpose, { id: string; label: string; description: string }>;

export function ProjectModelSettings({ projectId, purposes }: { projectId: string; purposes: readonly ModelDefaultPurpose[] }) {
  const defaults = useProjectModelDefaults(projectId);
  return <>
    {defaults.isPending && <p role="status">Loading project model defaults…</p>}
    {defaults.error && <p role="alert" className="settings-result settings-result-error">{String(defaults.error)}</p>}
    {defaults.data && purposes.map((purpose) => <ProjectDefault key={`${projectId}/${purpose}`} projectId={projectId} purpose={purpose} saved={defaults.data[purpose]} />)}
  </>;
}

function ProjectDefault({ projectId, purpose, saved }: { projectId: string; purpose: ModelDefaultPurpose; saved: ModelSelection | null }) {
  const [chosenProvider, setProvider] = useState<Provider>();
  const provider = chosenProvider ?? saved?.provider ?? "codex";
  const setting = projectModelSettings[purpose];
  const client = useQueryClient();
  const { options, loading, errors } = useLiveModelOptions([provider]);
  const selection = availableModelSelection(options, saved);
  const save = useMutation({
    mutationFn: (selection: ModelSelection | null) => invoke<ProjectModelDefaults>("set_project_model_default", { projectId, purpose, selection }),
    onSuccess: (defaults) => {
      client.setQueryData([...PROJECT_MODEL_DEFAULTS, projectId], defaults);
      return client.invalidateQueries({ queryKey: CHECKOUT_MODEL_DEFAULTS });
    },
  });
  return <SettingRow {...setting}>
    <div className="grid gap-2">
      <select id={setting.id} aria-label={`${setting.label} provider`} value={provider} disabled={save.isPending}
        onChange={(event) => { setProvider(event.currentTarget.value as Provider); save.reset(); }}>
        {Object.entries(providerLabels).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
      </select>
      <ModelPicker label={setting.label} options={options} loading={loading} disabled={save.isPending}
        selection={{ mode: "single", selected: selection, onChange: (model) => save.mutate(model),
          reset: { label: "Use profile default", onSelect: () => save.mutate(null) } }} />
      {saved && !loading && !selection && <p className="text-xs text-muted-foreground" role="status">
        Saved default · {providerLabels[saved.provider]} · {saved.modelId} · unavailable in the displayed profile.
      </p>}
      {(save.error ?? errors[0]) && <p role="alert" className="settings-result settings-result-error">{String(save.error ?? errors[0])}</p>}
    </div>
  </SettingRow>;
}

export function ExecutionProfileModelSettings({ provider }: { provider: Provider }) {
  const [open, setOpen] = useState(false);
  return <details onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary className="cursor-pointer text-sm">Execution profile model</summary>
    {open && <ProfileDefault provider={provider} />}
  </details>;
}

function ProfileDefault({ provider }: { provider: Provider }) {
  const [chosenProfile, setProfile] = useState<string>();
  const { options, loading, errors } = useLiveModelOptions([provider]);
  const profiles = [...new Set(options.map(({ selection }) => selection.profileId))];
  const profileId = chosenProfile && profiles.includes(chosenProfile) ? chosenProfile : profiles[0];
  const defaults = useExecutionProfileDefault(provider, profileId);
  const client = useQueryClient();
  const save = useMutation({
    mutationFn: (selection: ModelSelection | null) => invoke<ModelSelection | null>("set_execution_profile_default_model", { provider, profileId, selection }),
    onSuccess: (selection) => client.setQueryData([...EXECUTION_PROFILE_MODEL, provider, profileId], selection),
  });
  const scoped = options.filter(({ selection }) => selection.profileId === profileId);
  const selection = availableModelSelection(scoped, defaults.data);
  const error = save.error ?? defaults.error ?? errors[0];
  return <div className="mt-2 grid gap-2">
    {profiles.length > 1 && <label>Execution profile <select value={profileId} disabled={save.isPending}
      onChange={(event) => { setProfile(event.currentTarget.value); save.reset(); }}>
      {profiles.map((id) => <option value={id} key={id}>{id}</option>)}
    </select></label>}
    <ModelPicker label={`${providerLabels[provider]} profile default model`} options={scoped} loading={loading || Boolean(profileId && defaults.isPending)}
      disabled={!profileId || defaults.isPending || save.isPending}
      selection={{ mode: "single", selected: selection, onChange: (model) => save.mutate(model),
        reset: { label: "Choose on each use", onSelect: () => save.mutate(null) } }} />
    {defaults.data && !selection && <p role="status" className="text-xs text-muted-foreground">Saved model {defaults.data.modelId} is unavailable for this profile.</p>}
    {error && <p role="alert" className="settings-result settings-result-error">{String(error)}</p>}
  </div>;
}
