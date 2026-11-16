import { useQueries } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import { ModelPicker } from "@/components/model-picker";
import { participantModel, participantProviderNames, type TaskParticipantAttempt } from "@/task-participants";

type AgentProfile = Pick<TaskParticipantAttempt, "provider" | "profileId">;

export type ParticipantFilter = {
  agent: string;
  provider: Provider | "";
  model: ModelSelection | null;
};

export function hasParticipantFilter({ agent, provider, model }: ParticipantFilter) {
  return Boolean(agent || provider || model);
}

function agentKey({ provider, profileId }: AgentProfile) {
  return JSON.stringify([provider, profileId]);
}

function modelKey({ provider, profileId, modelId }: ModelSelection) {
  return JSON.stringify([provider, profileId, modelId]);
}

export function matchesParticipant(attempt: TaskParticipantAttempt, { agent, provider, model }: ParticipantFilter) {
  return (!agent || agentKey(attempt) === agent) && (!provider || attempt.provider === provider) &&
    (!model || (agentKey(model) === agentKey(attempt) && model.modelId === participantModel(attempt)));
}

const providers = Object.keys(participantProviderNames) as Provider[];

function filterModels(catalog: ModelOption[], attempts: readonly TaskParticipantAttempt[]) {
  const options = new Map(catalog.map((option) => [modelKey(option.selection), option]));
  for (const attempt of attempts) {
    const modelId = participantModel(attempt);
    if (!modelId) continue;
    const { provider, profileId } = attempt;
    const selection = { provider, profileId, modelId };
    const key = modelKey(selection);
    const current = options.get(key);
    if (current) {
      if (!current.selectable) options.set(key, { ...current, selectable: true });
    } else {
      options.set(key, { selection, name: `${modelId} (recorded)`, selectable: true,
        unavailableReason: null, rating: null, catalogVersion: 0 });
    }
  }
  const nameCounts = new Map<string, number>();
  for (const { name } of options.values()) nameCounts.set(name, (nameCounts.get(name) ?? 0) + 1);
  return [...options.values()].map((option) => (nameCounts.get(option.name) ?? 0) > 1 ? {
    ...option, name: `${option.name} · ${participantProviderNames[option.selection.provider]} · ${option.selection.profileId}`,
  } : option);
}

export function AgileParticipantFilters({ attempts, value, onChange }: {
  attempts: readonly TaskParticipantAttempt[];
  value: ParticipantFilter;
  onChange: (filters: ParticipantFilter) => void;
}) {
  const catalogs = useQueries({ queries: providers.map((provider) => ({
    queryKey: ["conversation-models", provider],
    queryFn: () => invoke<{ option: ModelOption }[]>("conversation_models", { provider }),
    staleTime: 60_000,
  })) });
  const options = filterModels(catalogs.flatMap(({ data }) => data?.map(({ option }) => option) ?? []), attempts);
  const agents = new Map(attempts.map(({ provider, profileId }) => [agentKey({ provider, profileId }), { provider, profileId }]));

  return <>
    <select aria-label="Filter by agent" className="delivery-filter" value={value.agent}
      onChange={(event) => onChange({ ...value, agent: event.currentTarget.value })}>
      <option value="">All agents</option>
      {[...agents].map(([key, { provider, profileId }]) => <option key={key} value={key}>
        {participantProviderNames[provider]} · {profileId}
      </option>)}
    </select>
    <select aria-label="Filter by provider" className="delivery-filter" value={value.provider}
      onChange={(event) => onChange({ ...value, provider: event.currentTarget.value as ParticipantFilter["provider"] })}>
      <option value="">All providers</option>
      {providers.map((provider) => <option key={provider} value={provider}>{participantProviderNames[provider]}</option>)}
    </select>
    <ModelPicker label="Filter by model" layout="inline" options={options}
      loading={!options.length && catalogs.some(({ isPending }) => isPending)}
      selection={{ mode: "single", selected: value.model, onChange: (model) => onChange({ ...value, model }),
        reset: { label: "All models", onSelect: () => onChange({ ...value, model: null }) } }}
      previewDetails={({ selection }) => <p className="px-3 pb-2 text-xs break-all text-muted-foreground">
        {participantProviderNames[selection.provider]} · {selection.profileId} · {selection.modelId}
      </p>} />
  </>;
}
