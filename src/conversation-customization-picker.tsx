import { useId, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import type { SessionMode } from "@/bindings/SessionMode";

export type ComposerCustomization = {
  marker: string;
  availability: "filesystem" | "provider" | "recorded_provider";
  skill: { id: string; provider: Provider; name: string; path: string; namespace: string | null; revision: string };
};

const skillToken = /^@skill:[0-9a-f]{64}:[0-9a-f]{64}$/;

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
  return hasComposerSkills(draft)
    ? draft.split(/(\s+)/).map((part) => skillToken.test(part) ? "[Skill invocation]" : part).join("")
    : draft;
}

function completion(text: string, cursor: number) {
  if (cursor < 0 || cursor > text.length || /\S/.test(text[cursor] ?? "")) return;
  const token = /(?:^|\s)([@/][\w.:-]*)$/.exec(text.slice(0, cursor))?.[1];
  if (!token) return;
  const start = cursor - token.length;
  if (token.startsWith("@skill:") || "@conversation".startsWith(token) && token.length > 3 || token.startsWith("@conversation")) return;
  if (token.startsWith("/") && (start !== 0 || ["/btw", "/goal"].some((command) => command.startsWith(token) && token.length > 1))) return;
  return { token, start, end: cursor };
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
  const trigger = completion(draft, cursor);
  const completionKey = trigger && JSON.stringify([draft, cursor]);
  const pinnedTokens = draft.split(/\s+/).filter((token) => skillToken.test(token));
  const active = enabled && Boolean(selection && (manual || (trigger && dismissed !== completionKey)));
  const query = useQuery({
    ...composerCustomizationsQuery(conversationId, selection, effort, mode),
    enabled: enabled && Boolean(selection && (active || pinnedTokens.length)),
  });
  const available = query.data ?? [];
  const needle = (manual ? "" : trigger?.token.slice(1) ?? "").toLocaleLowerCase();
  const options = active ? available.filter(({ skill, marker }) => !pinnedTokens.includes(marker) &&
    [skill.name, skill.namespace, skill.path].some((value) => value?.toLocaleLowerCase().includes(needle))).slice(0, 20) : [];
  const highlighted = options.length ? Math.min(index, options.length - 1) : 0;

  function dismiss() {
    setManual(false);
    setDismissed(completionKey);
    setIndex(0);
  }

  return {
    listId, active, options, highlighted, canOpen: enabled && Boolean(selection), error: query.error, loading: query.isPending,
    pinned: pinnedTokens.map((token) => ({ token, option: available.find(({ marker }) => marker === token) })),
    dismiss,
    open() { setManual(true); setDismissed(undefined); setIndex(0); },
    move(delta: number) {
      if (options.length) setIndex((highlighted + delta + options.length) % options.length);
    },
    pick(option: ComposerCustomization) {
      const selected = options.find(({ marker }) => marker === option.marker);
      if (!selected) return { text: draft, cursor };
      const range = trigger ?? { start: cursor, end: cursor };
      const prefix = draft.slice(0, range.start);
      const before = /\S$/.test(prefix) ? `${prefix} ` : prefix;
      const insert = `${selected.marker} `;
      const after = draft.slice(range.end).replace(/^ /, "");
      dismiss();
      return { text: `${before}${insert}${after}`, cursor: before.length + insert.length };
    },
    remove(token: string) {
      return draft.split(/(\s+)/).filter((part) => part !== token).join("").trimStart();
    },
  };
}

type Picker = ReturnType<typeof useComposerCustomizations>;

function actionLabel(option: ComposerCustomization) {
  const { name, namespace } = option.skill;
  return namespace ? `${namespace} · ${name}` : name;
}

const availability = {
  provider: "Reported by CLI",
  recorded_provider: "Previously reported · rechecked before dispatch",
  filesystem: "Discovered · rechecked by CLI before dispatch",
};

export function ComposerCustomizationPicker({ picker, onPick, onRemove }: {
  picker: Picker;
  onPick: (option: ComposerCustomization) => void;
  onRemove: (token: string) => void;
}) {
  return <div className="grid gap-1">
    {picker.pinned.length > 0 && <ul aria-label="Selected skills and plugin exports" className="grid gap-1">
      {picker.pinned.map(({ token, option }, index) => <li key={`${token}:${index}`} className="flex flex-wrap items-center gap-x-2 break-all text-xs">
        {option ? <><strong>{actionLabel(option)}</strong><span>{option.skill.path}</span>
          <code title={option.skill.revision}>Revision {option.skill.revision.slice(0, 12)}</code></> :
          <strong>{picker.loading ? "Loading skill selection…" : "Skill selection unavailable for this profile"}</strong>}
        <button type="button" className="conv-quiet-button" onClick={() => onRemove(token)}
          aria-label={option ? `Remove ${actionLabel(option)}` : "Remove unavailable skill"}>Remove</button>
      </li>)}
    </ul>}
    {!picker.active && <button type="button" className="conv-quiet-button justify-self-start" disabled={!picker.canOpen}
      onClick={picker.open}>Skills & plugin exports</button>}
    {picker.active && <div className="pipeline-evidence grid gap-1">
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
          role="option" aria-selected={index === picker.highlighted} onClick={() => onPick(option)}
          className="grid gap-0.5 break-all rounded px-2 py-1 text-left aria-selected:bg-accent">
          <strong className="text-sm">{actionLabel(option)}</strong>
          <span className="text-xs">Codex skill input · {option.skill.path}</span>
          <small className="text-muted-foreground">{availability[option.availability]}</small>
        </button>)}
      </div>}
    </div>}
  </div>;
}
