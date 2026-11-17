import { useQueries } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Blocks, X } from "lucide-react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import {
  composerActionLabel, hasComposerSkills, readPipelineActionMarker, type ComposerCustomization,
} from "@/conversation-customization-picker";

export type PipelineComposerAgent = {
  nodeId: string;
  label: string;
  selection?: ModelSelection | null;
  effort?: string | null;
  mode?: "read_only" | "plan";
  actionEligible?: boolean;
};

type Action = { nodeId: string; marker: string };

type PinnedAction = {
  token: string;
  action: Action;
  agent?: PipelineComposerAgent;
  option?: ComposerCustomization;
  unavailable?: string;
};

export function usePipelineComposerCustomizations({ conversationId, agents, selection, effort, text, query, active, enabled }: {
  conversationId: string;
  agents?: readonly PipelineComposerAgent[];
  selection: ModelSelection | null;
  effort: string | null;
  text: string;
  query: string;
  active: boolean;
  enabled: boolean;
}) {
  const parts = text.split(/(\s+)/);
  const markers = enabled ? parts.flatMap((token) => {
    const action = readPipelineActionMarker(token);
    return action ? [{ token, action }] : [];
  }) : [];
  const targets = (agents ?? []).filter(({ actionEligible }) => actionEligible !== false).map((agent) => {
    const scope = { selection: agent.selection ?? selection, effort: agent.selection ? agent.effort ?? null : effort,
      mode: agent.mode ?? "read_only" };
    return { agent, scope, key: JSON.stringify(scope) };
  });
  const scopes = [...new Map(targets.map(({ key, scope }) => [key, scope]))];
  const lookups = useQueries({ queries: scopes.map(([, scope]) => ({
    queryKey: ["conversation", conversationId, "composer-customizations", scope.selection, scope.effort, scope.mode],
    queryFn: () => invoke<ComposerCustomization[]>("composer_customizations", { conversationId, ...scope }),
    enabled: enabled && Boolean(scope.selection) && (active || markers.length > 0),
    staleTime: 3000,
    retry: false,
  })) });
  const byScope = new Map(scopes.map(([key], index) => [key, lookups[index]]));
  const options = active && enabled ? targets.flatMap(({ agent, scope, key }) => {
    if (!scope.selection) return [];
    return (byScope.get(key)?.data ?? []).filter((option) => [option.skill.name, option.skill.namespace, option.skill.path]
      .some((value) => value?.toLocaleLowerCase().includes(query.toLocaleLowerCase()))).map((option) => ({ agent, option }));
  }).slice(0, 24) : [];
  const pinned: PinnedAction[] = markers.map(({ token, action }) => {
    const target = targets.find(({ agent }) => agent.nodeId === action.nodeId);
    const lookup = target && byScope.get(target.key);
    const option = lookup?.data?.find(({ marker }) => marker === action.marker);
    const unavailable = !agents ? "Loading pipeline agent stages…"
      : !target ? "The selected stage is unavailable in this pipeline revision."
      : !target.scope.selection ? "Choose a model for this stage to check its actions."
      : lookup?.error ? "Could not check actions for the selected stage."
      : lookup?.isPending ? "Checking the action for this stage…"
      : option?.unavailableReason ?? (!option ? "The selected action or revision is unavailable for this stage." : undefined);
    return { token, action, option, unavailable, agent: target?.agent };
  });
  const cleaned = markers.length ? parts.filter((part) => !readPipelineActionMarker(part)).join("").trim() : text;
  const first = pinned[0];
  const blocker = !enabled ? undefined : hasComposerSkills(text) ? "Select this action again for a pipeline agent stage."
    : pinned.length > 1 ? "Remove extra actions. A pipeline turn can target one action at a time."
    : first?.unavailable ?? (first && !cleaned ? "Add a message for the pipeline to run." : undefined);
  const loading = enabled && active && targets.some(({ scope, key }) => Boolean(scope.selection) && byScope.get(key)?.isPending);
  const error = enabled && active && lookups.some(({ error }) => error) ? "Could not read every stage's action inventory." : undefined;
  return { options, pinned, loading, error,
    status: enabled && active ? !agents ? "Loading agent stages…" : !targets.length ? "This pipeline has no compatible agent stages."
      : !targets.some(({ scope }) => scope.selection) ? "Choose a default model to find actions for its stages." : undefined : undefined,
    submission: { text: cleaned, action: first?.action, blocker } };
}

export function PipelineComposerActionChips({ actions, onRemove }: { actions: readonly PinnedAction[]; onRemove: (token: string) => void }) {
  if (!actions.length) return null;
  return <ul aria-label="Selected pipeline actions" className="grid gap-1">
    {actions.map(({ token, option, agent, unavailable }, index) => <li key={`${token}:${index}`} className="flex items-start gap-2 rounded border border-border p-2 text-xs">
      <Blocks size={15} aria-hidden />
      <div className="min-w-0 flex-1 break-words">
        <p><strong>{option ? composerActionLabel(option) : "Pipeline action"}</strong>{agent && ` · ${agent.label}`}</p>
        {unavailable && <p role="status" className="text-muted-foreground">{unavailable}</p>}
      </div>
      <button type="button" className="conv-icon-button" aria-label={`Remove ${option ? composerActionLabel(option) : "pipeline action"}`} onClick={() => onRemove(token)}>
        <X size={14} aria-hidden />
      </button>
    </li>)}
  </ul>;
}
