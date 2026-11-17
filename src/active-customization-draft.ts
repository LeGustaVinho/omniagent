import { useSyncExternalStore } from "react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { SessionMode } from "@/bindings/SessionMode";

type ActiveDraft = {
  checkoutId: string;
  conversationId: string;
  title: string;
  selection: ModelSelection | null;
  effort: string | null;
  mode: SessionMode;
};

const drafts = new Map<string, ActiveDraft>();
const listeners = new Set<() => void>();

export function registerActiveCustomizationDraft(draft: ActiveDraft) {
  drafts.set(draft.checkoutId, draft);
  for (const notify of listeners) notify();
  return () => {
    if (drafts.get(draft.checkoutId) !== draft) return;
    drafts.delete(draft.checkoutId);
    for (const notify of listeners) notify();
  };
}

export function activeCustomizationDraft(checkoutId?: string) {
  return checkoutId ? drafts.get(checkoutId) : undefined;
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

export function useActiveCustomizationDraft(checkoutId?: string) {
  return useSyncExternalStore(subscribe, () => activeCustomizationDraft(checkoutId));
}
