import { useId, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Blocks } from "lucide-react";
import { composerCompletion, replaceComposerCompletion } from "@/composer-completion";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import type { SessionMode } from "@/bindings/SessionMode";

export type ComposerCustomization = {
  marker: string;
  availability: "filesystem" | "provider" | "recorded_provider";
  route?: "native_skill" | "markdown_context" | "native_command";
  unavailableReason?: string | null;
  skill: { id: string; provider: Provider; name: string; path: string; namespace: string | null; revision: string };
};

const skillToken = /^@(?:skill|command):[0-9a-f]{64}:[0-9a-f]{64}$/;
const stageToken = /^@stage:([^:\s]+):((?:skill|command):[0-9a-f]{64}:[0-9a-f]{64})$/;

export function composerCustomizationsQuery(conversationId: string, selection: ModelSelection | null, effort: string | null, mode: SessionMode) {
  return {
    queryKey: ["conversation", conversationId, "composer-customizations", selection, effort, mode],
    queryFn: () => invoke<ComposerCustomization[]>("composer_customizations", { conversationId, selection, effort, mode }),
    staleTime: 3000,
    retry: false as const,
  };
}

export function hasComposerSkills(draft: string) {
  return draft.split(/\s+/).some((token) => skillToken.test(token));
}

export function conversationSkillMessage(draft: string) {
  return draft.split(/(\s+)/).map((part) => {
    if (skillToken.test(part)) return part.startsWith("@command:") ? "[CLI command]" : "[Skill invocation]";
    return stageToken.test(part) ? "[Pipeline action]" : part;
  }).join("");
}

export function pipelineActionMarker(nodeId: string, marker: string) {
  return `@stage:${encodeURIComponent(nodeId)}:${marker.slice(1)}`;
}

export function readPipelineActionMarker(token: string) {
  const match = stageToken.exec(token);
  if (!match) return;
  try { return { nodeId: decodeURIComponent(match[1]!), marker: `@${match[2]}` }; }
  catch { return undefined; }
}

export function useComposerCustomizations({ conversationId, selection, effort, mode, draft, cursor, enabled }: {
  conversationId: string;
  selection: ModelSelection | null;
  effort: string | null;
  mode: SessionMode;
  draft: string;
  cursor: number;
  enabled: boolean;
}) {
  const listId = useId();
  const [manual, setManual] = useState(false);
  const [dismissed, setDismissed] = useState<string>();
  const [index, setIndex] = useState(0);
  const token = composerCompletion(draft, cursor);
  const trigger = token?.trigger === "/" ? token : undefined;
  const completionKey = trigger && JSON.stringify([draft, cursor]);
  const pinnedTokens = draft.split(/\s+/).filter((token) => skillToken.test(token));
  const active = enabled && Boolean(selection && (manual || (trigger && dismissed !== completionKey)));
  const query = useQuery({
    ...composerCustomizationsQuery(conversationId, selection, effort, mode),
    enabled: enabled && Boolean(selection && (active || pinnedTokens.length)),
  });
  const available = query.data ?? [];
  const needle = (manual ? "" : trigger?.query ?? "").toLocaleLowerCase();
  const options = active ? available.filter(({ skill, marker }) => !pinnedTokens.includes(marker) &&
    [skill.name, skill.namespace, skill.path].some((value) => value?.toLocaleLowerCase().includes(needle))).slice(0, 20) : [];
  const highlighted = options.length ? Math.min(index, options.length - 1) : 0;

  function dismiss() {
    setManual(false);
    setDismissed(completionKey);
    setIndex(0);
  }

  return {
    listId, active, manual, options, highlighted, canOpen: enabled && Boolean(selection), error: query.error, loading: query.isPending,
    pinned: pinnedTokens.map((token) => ({ token, option: available.find(({ marker, unavailableReason }) => marker === token && !unavailableReason) })),
    dismiss,
    open() { setManual(true); setDismissed(undefined); setIndex(0); },
    move(delta: number) {
      if (options.length) setIndex((highlighted + delta + options.length) % options.length);
    },
    pick(option: ComposerCustomization) {
      const selected = options.find(({ marker }) => marker === option.marker);
      if (!selected || selected.unavailableReason) return { text: draft, cursor };
      const position = cursor < 0 ? draft.length : cursor;
      const range = trigger ?? { start: position, end: position };
      dismiss();
      return replaceComposerCompletion(draft, range, selected.marker);
    },
    remove(token: string) {
      return draft.split(/(\s+)/).filter((part) => part !== token).join("").trimStart();
    },
  };
}

type Picker = ReturnType<typeof useComposerCustomizations>;

export function composerActionLabel(option: ComposerCustomization) {
  const { name, namespace } = option.skill;
  return namespace ? `${namespace} · ${name}` : name;
}

const availability = {
  provider: "Reported by CLI",
  recorded_provider: "Previously reported · rechecked before dispatch",
  filesystem: "Discovered · rechecked by CLI before dispatch",
};

export function composerActionDetail(option: ComposerCustomization) {
  const route = option.route === "native_command" ? "Native CLI command"
    : option.route === "markdown_context" ? "Pinned Markdown context" : "Codex skill input";
  return [route, option.skill.path, availability[option.availability]].filter(Boolean).join(" · ");
}

export function ComposerCustomizationPicker({ picker, onPick, onRemove, showOptions = true, onOpen }: {
  picker: Picker;
  onPick: (option: ComposerCustomization) => void;
  onRemove: (token: string) => void;
  showOptions?: boolean;
  onOpen?: () => void;
}) {
  return <div className="conv-skill-tool grid gap-1">
    {(!picker.active || !showOptions) && <button type="button" className="conv-quiet-button conv-composer-tool justify-self-start" disabled={!picker.canOpen}
      onClick={onOpen ?? picker.open}><Blocks size={15} aria-hidden /> Skills & plugin exports</button>}
    {picker.pinned.length > 0 && <ul aria-label="Selected skills and plugin exports" className="grid gap-1">
      {picker.pinned.map(({ token, option }, index) => <li key={`${token}:${index}`} className="flex flex-wrap items-center gap-x-2 break-all text-xs">
        {option ? <><strong>{composerActionLabel(option)}</strong><span>{option.skill.path || composerActionDetail(option)}</span>
          {option.skill.revision && <code title={option.skill.revision}>Revision {option.skill.revision.slice(0, 12)}</code>}</> :
          <strong>{picker.loading ? "Loading skill selection…" : "Skill selection unavailable for this profile"}</strong>}
        <button type="button" className="conv-quiet-button" onClick={() => onRemove(token)}
          aria-label={option ? `Remove ${composerActionLabel(option)}` : "Remove unavailable skill"}>Remove</button>
      </li>)}
    </ul>}
    {showOptions && picker.active && <div className="pipeline-evidence grid gap-1">
      <div className="flex justify-between gap-2 text-sm"><strong>Skills & plugin exports</strong>
        <button type="button" className="conv-quiet-button" onClick={picker.dismiss}>Close picker</button></div>
      {picker.loading && <p role="status" className="text-sm">Discovering customizations…</p>}
      {picker.error && <p role="alert" className="text-sm">{String(picker.error)}</p>}
      {!picker.loading && !picker.error && !picker.options.length && <p className="text-sm text-muted-foreground">
        No matching invocations for this profile and worktree.
      </p>}
      {picker.options.length > 0 && <div id={picker.listId} role="listbox" aria-label="Skill and plugin invocations"
        className="grid max-h-44 gap-1 overflow-auto">
        {picker.options.map((option, index) => <button type="button" key={option.marker} id={`${picker.listId}-${index}`}
          role="option" aria-selected={index === picker.highlighted} disabled={Boolean(option.unavailableReason)} onClick={() => onPick(option)}
          className="grid gap-0.5 break-all rounded px-2 py-1 text-left aria-selected:bg-accent">
          <strong className="text-sm">{composerActionLabel(option)}</strong>
          <span className="text-xs">{composerActionDetail(option)}</span>
          {option.unavailableReason && <small className="text-muted-foreground">{option.unavailableReason}</small>}
        </button>)}
      </div>}
    </div>}
  </div>;
}
