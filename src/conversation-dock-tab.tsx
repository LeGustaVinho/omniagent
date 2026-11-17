import { invoke } from "@tauri-apps/api/core";
import { DockviewDefaultTab, type IDockviewPanelHeaderProps } from "dockview-react";
import { createContext, useCallback, useContext, useEffect, useRef, useSyncExternalStore } from "react";
import { ContextActions } from "@/components/ui/context-actions";
import { useConversationTitle } from "@/conversation-library";
import { useConversationTranscriptExport } from "@/conversation-transcript-export";

export type ConversationTabFeedback = { text: string; failed?: boolean };
export const ConversationDockTabContext = createContext<{
  checkoutId: string;
  report: (feedback?: ConversationTabFeedback) => void;
} | null>(null);

type ConversationTabParams = { conversationId?: string; title?: string };

export function ConversationDockTab(props: IDockviewPanelHeaderProps<ConversationTabParams>) {
  const { api, containerApi, params, tabLocation } = props;
  const scope = useContext(ConversationDockTabContext);
  const anchor = useRef<HTMLDivElement>(null);
  const conversationId = params.conversationId ?? "";
  const isConversation = Boolean(conversationId && api.id === `conversation:${conversationId}`);
  const saved = useConversationTitle(isConversation ? scope?.checkoutId : undefined, conversationId);
  const conversation = saved.isSuccess && saved.data.id === conversationId && saved.data.checkoutId === scope?.checkoutId ? saved.data : undefined;
  const exportTranscript = useConversationTranscriptExport((text) => scope?.report(text ? { text } : undefined));
  const subscribe = useCallback((notify: () => void) => {
    const change = containerApi.onDidLayoutChange(notify);
    return () => change.dispose();
  }, [containerApi]);
  const position = useSyncExternalStore(subscribe, () => {
    const members = api.group.panels;
    return `${members.findIndex(({ id }) => id === api.id)}:${members.length}`;
  });
  const [index = -1, count = 0] = position.split(":").map(Number);

  useEffect(() => {
    const content = anchor.current;
    const tab = content?.closest<HTMLElement>('[role="tab"]');
    const view = tab?.ownerDocument.defaultView;
    if (!content || !tab || !view) return;

    // Dockview owns the tab's focus and outer hit area. Route its own menu events
    // to the same menu as events over the tab contents.
    const keyMenu = (event: KeyboardEvent) => {
      if (event.target !== tab || (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10"))) return;
      const button = content.parentElement?.querySelector<HTMLButtonElement>(".context-actions-more");
      if (!button) return;
      event.preventDefault();
      event.stopPropagation();
      button.focus();
      button.dispatchEvent(new view.KeyboardEvent("keydown", { key: event.key, shiftKey: event.shiftKey, bubbles: true, cancelable: true }));
    };
    const pointerMenu = (event: MouseEvent) => {
      if (event.target !== tab) return;
      event.preventDefault();
      event.stopPropagation();
      content.dispatchEvent(new view.MouseEvent("contextmenu", {
        bubbles: true, cancelable: true, button: event.button, clientX: event.clientX, clientY: event.clientY,
      }));
    };
    tab.addEventListener("keydown", keyMenu);
    tab.addEventListener("contextmenu", pointerMenu);
    return () => {
      tab.removeEventListener("keydown", keyMenu);
      tab.removeEventListener("contextmenu", pointerMenu);
    };
  }, [isConversation, position, tabLocation]);

  function reportError(error: unknown) {
    scope?.report({ text: String(error), failed: true });
  }

  function perform(action: () => Promise<unknown>, success?: string) {
    scope?.report();
    void Promise.resolve().then(action).then(() => { if (success) scope?.report({ text: success }); }, reportError);
  }

  function moveTab(offset: number) {
    const group = api.group;
    const current = group.panels.findIndex(({ id }) => id === api.id);
    const next = current + offset;
    if (current >= 0 && next >= 0 && next < group.panels.length) api.moveTo({ group, position: "center", index: next });
  }

  if (!isConversation) return <DockviewDefaultTab {...props} />;
  const title = conversation?.title ?? api.title ?? params.title ?? conversationId;
  return <ContextActions label={`${title} tab`} title={title} stopPointerDown groups={[
    conversation ? [
      { label: "Copy conversation ID", run: () => perform(() => navigator.clipboard.writeText(conversation.id), "Conversation ID copied.") },
      { label: "Open worktree folder", run: () => perform(() => invoke("open_checkout_item_in_shell", {
        checkoutId: conversation.checkoutId, relativePath: "", action: "open",
      })) },
      { label: "Export conversation as Markdown…", disabled: exportTranscript.isPending,
        run: () => { void exportTranscript.mutateAsync(conversation).catch(reportError); } },
    ] : [],
    [
      { label: "Move tab left", disabled: index <= 0, run: () => moveTab(-1) },
      { label: "Move tab right", disabled: index < 0 || index >= count - 1, run: () => moveTab(1) },
      { label: "Split tab right", disabled: index < 0 || count <= 1, run: () => {
        const group = api.group;
        if (group.panels.length > 1) api.moveTo({ group, position: "right" });
      } },
    ],
    [{ label: "Close tab", run: () => api.close() }],
  ]}>
    <div ref={anchor} className="min-w-0"><DockviewDefaultTab {...props} /></div>
  </ContextActions>;
}
