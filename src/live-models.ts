import { queryOptions, useQueries } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";

export type LiveModel = { option: ModelOption; efforts: string[]; defaultEffort: string | null };
export const LIVE_MODEL_PROVIDERS: readonly Provider[] = ["codex", "claude", "antigravity"];

export function liveModelsQuery(provider: Provider, modelId?: string) {
  return queryOptions({
    queryKey: modelId ? ["conversation-models", provider, modelId] : ["conversation-models", provider],
    queryFn: () => invoke<LiveModel[]>("conversation_models", modelId ? { provider, modelId } : { provider }),
    retry: false,
    staleTime: 60_000,
  });
}

export function useLiveModelOptions(providers: readonly Provider[] = LIVE_MODEL_PROVIDERS, enabled = true) {
  const queries = useQueries({ queries: providers.map((provider) => ({ ...liveModelsQuery(provider), enabled })) });
  const models = queries.flatMap(({ data }) => data ?? []);
  return {
    models,
    options: models.map(({ option }) => option),
    loading: queries.some(({ isLoading }) => isLoading),
    errors: queries.flatMap(({ error }) => error ? [error] : []),
  };
}

export function sameModelSelection(left: ModelSelection, right?: ModelSelection | null) {
  return left.provider === right?.provider && left.profileId === right.profileId && left.modelId === right.modelId;
}

export function availableModelSelection(options: readonly ModelOption[], ...candidates: (ModelSelection | null | undefined)[]) {
  return candidates.find((candidate) => candidate && options.some(({ selectable, selection }) => selectable && sameModelSelection(selection, candidate))) ?? null;
}
