import { invoke } from "@tauri-apps/api/core";
import {
  DockviewReact,
  themeDark,
  type DockviewApi,
  type DockviewReadyEvent,
  type IDockviewHeaderActionsProps,
} from "dockview-react";
import { ArrowLeft, ArrowRight, Columns2, GitBranch } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { ConversationDockTab, ConversationDockTabContext, type ConversationTabFeedback } from "@/conversation-dock-tab";
import { ConversationLibrary, ConversationLibraryNavigation, type OpenLibraryConversation, useConversationLibraryRead } from "@/conversation-library";
import { ConversationEmptyState, ConversationNavigation, ConversationWorkspaceLayout } from "@/conversation-layout";
import { ConversationListPanel, ConversationPanel } from "@/conversation-panels";
import { useNewConversation } from "@/conversation-pipeline-picker";
import { DesktopDiagnostics, DiagnosticsWorkspace, type DiagnosticView } from "@/diagnostics-workspace";
import { CustomizationsPanel } from "@/customizations-panel";
import { FixtureListPanel, FixtureRunPanel } from "@/fixture-panels";
import { ProviderSettingsPanel } from "@/provider-settings";
import {
  openConversation,
  openFixtureList,
  persistFixtureLayout,
  restoreDesktopLayout,
  type DesktopDockArea,
} from "@/workspace-tabs";
import { Button } from "@/components/ui/button";
import "dockview/dist/styles/dockview.css";

const legacyPanels = {
  fixtures: FixtureListPanel,
  run: FixtureRunPanel,
  providerSettings: ProviderSettingsPanel,
  customizations: CustomizationsPanel,
  conversations: ConversationListPanel,
  conversation: ConversationPanel,
};
const noBuiltInTabMenu = () => [];

function TabActions({ activePanel, panels, group }: IDockviewHeaderActionsProps) {
  if (!activePanel || panels.length < 2) return null;
  const index = panels.indexOf(activePanel);
  return (
    <div className="flex items-center gap-0.5 px-1">
      <Button className="h-7 w-7 p-0" aria-label="Move tab left" title="Move tab left" disabled={index <= 0} variant="ghost"
        onClick={() => activePanel.api.moveTo({ group, position: "center", index: index - 1 })}><ArrowLeft size={14} /></Button>
      <Button className="h-7 w-7 p-0" aria-label="Move tab right" title="Move tab right" disabled={index >= panels.length - 1} variant="ghost"
        onClick={() => activePanel.api.moveTo({ group, position: "center", index: index + 1 })}><ArrowRight size={14} /></Button>
      <Button className="h-7 w-7 p-0" aria-label="Split right" title="Split right" variant="ghost"
        onClick={() => activePanel.api.moveTo({ group, position: "right" })}><Columns2 size={14} /></Button>
    </div>
  );
}

function useDesktopDock(checkout: ProjectCheckout, area: DesktopDockArea) {
  const [api, setApi] = useState<DockviewApi>();
  const [ready, setReady] = useState(false);
  const [activeId, setActiveId] = useState<string>();
  const [hasPanels, setHasPanels] = useState(false);
  const [layoutError, setLayoutError] = useState("");

  useEffect(() => {
    if (!api) return;
    let disposed = false;
    let release: (() => void) | undefined;
    let updating = false;
    let pending: string | undefined;
    const scope = { checkoutId: checkout.checkoutId, workspace: area };

    function syncSelection() {
      setActiveId(api?.activePanel?.id.slice("conversation:".length));
      setHasPanels(Boolean(api?.panels.length));
    }

    function writeLayout(layout: string) {
      pending = layout;
      if (updating) return;
      updating = true;
      async function write() {
        try {
          while (pending !== undefined) {
            const next = pending;
            pending = undefined;
            try {
              await invoke("save_workbench_layout", { ...scope, layout: next });
              if (!disposed) setLayoutError("");
            } catch (error) {
              if (!disposed) setLayoutError(String(error));
            }
          }
        } finally {
          updating = false;
        }
      }
      void write();
    }

    const layout = api.onDidLayoutChange(syncSelection);
    const selection = api.onDidActivePanelChange(syncSelection);

    async function restore() {
      if (!api) return;
      let stored: string | null = null;
      let legacy: string | null = null;
      let canPersistMigration = true;
      try {
        stored = await invoke<string | null>("workbench_layout", scope);
        if (stored === null) legacy = await invoke<string | null>("workbench_layout", { ...scope, workspace: "fixtures" });
      } catch (error) {
        canPersistMigration = false;
        if (!disposed) setLayoutError(String(error));
      }
      if (disposed) return;
      restoreDesktopLayout(api, checkout, area, stored, legacy);
      release = persistFixtureLayout(api, checkout, writeLayout, area);
      if (canPersistMigration && legacy !== null) writeLayout(JSON.stringify(api.toJSON()));
      syncSelection();
      setReady(true);
    }

    void restore();
    return () => {
      disposed = true;
      layout.dispose();
      selection.dispose();
      release?.();
    };
  }, [api, area, checkout]);

  return { api, onReady: ({ api }: DockviewReadyEvent) => setApi(api), ready, activeId, hasPanels, layoutError };
}

function Dock({
  dock, error, conversationCheckoutId, children,
}: { dock: ReturnType<typeof useDesktopDock>; error?: string; conversationCheckoutId?: string; children?: React.ReactNode }) {
  const [feedback, report] = useState<ConversationTabFeedback>();
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      {dock.layoutError && <p role="status" className="work-inline px-4 py-2">Layout storage: {dock.layoutError}</p>}
      {error && <p role="alert" className="conv-alert mx-4 my-2">{error}</p>}
      {feedback && <p role={feedback.failed ? "alert" : "status"}
        className={feedback.failed ? "conv-alert mx-4 my-2" : "conv-sidebar-note px-4"}>{feedback.text}</p>}
      <div className="workspace-dock relative min-h-0 flex-1">
        <ConversationDockTabContext.Provider value={conversationCheckoutId ? { checkoutId: conversationCheckoutId, report } : null}>
          <DockviewReact components={legacyPanels} onReady={dock.onReady} theme={themeDark} disableFloatingGroups rightHeaderActionsComponent={TabActions}
            defaultTabComponent={conversationCheckoutId ? ConversationDockTab : undefined}
            getTabContextMenuItems={conversationCheckoutId ? noBuiltInTabMenu : undefined} />
        </ConversationDockTabContext.Provider>
        {!dock.ready && <div className="work-dock-empty" role="status">Restoring tabs…</div>}
        {dock.ready && !dock.hasPanels && children && <div className="work-dock-empty">{children}</div>}
      </div>
    </div>
  );
}

export function ConversationsWorkspace({
  checkout, sidebar, onSidebarChange, compact, target, onOpenOtherCheckout, onOpenCheckoutInIde,
}: {
  checkout: ProjectCheckout;
  sidebar: boolean;
  onSidebarChange: (visible: boolean) => void;
  compact: boolean;
  target?: string;
  onOpenOtherCheckout?: OpenLibraryConversation;
  onOpenCheckoutInIde?: (checkout: ProjectCheckout) => Promise<void>;
}) {
  const dock = useDesktopDock(checkout, "conversations");
  const activeScope = useRef<{ api?: DockviewApi; checkoutId: string }>(null);
  useEffect(() => {
    activeScope.current = { api: dock.api, checkoutId: checkout.checkoutId };
    return () => { activeScope.current = null; };
  }, [checkout.checkoutId, dock.api]);
  const [targetError, setTargetError] = useState("");
  const markRead = useConversationLibraryRead();
  useEffect(() => {
    if (dock.ready && dock.activeId) markRead(checkout.checkoutId, dock.activeId);
  }, [checkout.checkoutId, dock.activeId, dock.ready, markRead]);
  useEffect(() => {
    if (!dock.ready || !dock.api || !target) return;
    const api = dock.api;
    setTargetError("");
    const existing = api.getPanel("conversation:" + target);
    if (existing) return existing.api.setActive();
    let cancelled = false;
    void invoke<Conversation>("checkout_conversation", { checkoutId: checkout.checkoutId, conversationId: target }).then((conversation) => {
      if (!cancelled) openConversation(api, conversation);
    }).catch((error: unknown) => { if (!cancelled) setTargetError(String(error)); });
    return () => { cancelled = true; };
  }, [checkout.checkoutId, dock.api, dock.ready, target]);

  function open(conversation: Conversation) {
    if (!dock.api) return;
    openConversation(dock.api, conversation);
    if (compact) onSidebarChange(false);
  }

  const start = useNewConversation(checkout.checkoutId, open);

  async function openLibraryConversation(source: ProjectCheckout, conversationId: string, beside = false) {
    if (source.projectId !== checkout.projectId) throw new Error("This conversation belongs to another project.");
    const scope = activeScope.current;
    const api = scope?.api;
    if (!scope || !api || scope.checkoutId !== checkout.checkoutId) return;
    if (source.checkoutId !== checkout.checkoutId) {
      if (beside || !onOpenOtherCheckout) throw new Error("Switch to the source worktree to open this conversation.");
      await onOpenOtherCheckout(source, conversationId);
      return;
    }
    const reference = beside ? api.activePanel?.id : undefined;
    const panel = api.getPanel("conversation:" + conversationId);
    if (panel && !beside) {
      if (conversationId === dock.activeId) markRead(checkout.checkoutId, conversationId);
      panel.api.setActive();
      if (compact) onSidebarChange(false);
      return;
    }
    const conversation = await invoke<Conversation>("checkout_conversation", { checkoutId: source.checkoutId, conversationId });
    if (activeScope.current !== scope) return;
    if (conversation.checkoutId !== scope.checkoutId || conversation.id !== conversationId) {
      throw new Error("This conversation is unavailable in the selected worktree.");
    }
    openConversation(api, conversation, reference);
    if (conversationId === dock.activeId) markRead(checkout.checkoutId, conversationId);
    if (compact) onSidebarChange(false);
  }

  const contents = dock.ready
    ? <ConversationLibrary checkout={checkout} onOpen={openLibraryConversation} activeId={dock.activeId}
      onOpenBeside={dock.hasPanels ? (source, id) => openLibraryConversation(source, id, true) : undefined}
      onOpenCheckoutInIde={onOpenCheckoutInIde}
      onStart={start.choose} startPending={start.isPending} />
    : <p className="work-progress" role="status">Loading conversations…</p>;
  return (
    <ConversationLibraryNavigation.Provider value={{
      checkoutId: checkout.checkoutId, projectId: checkout.projectId, open: openLibraryConversation,
    }}>
      <ConversationWorkspaceLayout navigation={<ConversationNavigation compact={compact} open={sidebar} onOpenChange={onSidebarChange}>
        {contents}
      </ConversationNavigation>}>
        <Dock dock={dock} error={targetError} conversationCheckoutId={checkout.checkoutId}>
          <ConversationEmptyState title="Start a conversation" description="Ask an agent about this checkout or open an existing conversation.">
            <Button onClick={start.choose} disabled={start.isPending} aria-label="Start a new conversation">
              {start.isPending ? "Creating…" : "New conversation"}
            </Button>
            {!sidebar && <Button variant="outline" onClick={() => onSidebarChange(true)}>Show conversations</Button>}
          </ConversationEmptyState>
        </Dock>
      </ConversationWorkspaceLayout>
      {start.dialog}
    </ConversationLibraryNavigation.Provider>
  );
}

export function DiagnosticsDock({ checkout, openSystem }: { checkout: ProjectCheckout; openSystem?: () => void }) {
  const dock = useDesktopDock(checkout, "diagnostics");
  const [view, setView] = useState<DiagnosticView>("desktop");
  const [fixturesVisited, setFixturesVisited] = useState(false);

  function select(next: DiagnosticView) {
    if (next === "fixture") {
      if (view === next && dock.ready && dock.api) openFixtureList(dock.api);
      setFixturesVisited(true);
    }
    setView(next);
  }

  return <DiagnosticsWorkspace active={view} onSelect={select}
    desktop={<DesktopDiagnostics openSystem={openSystem} />}
    fixtures={fixturesVisited && <Dock dock={dock}>
      <span className="work-empty-icon"><GitBranch size={23} aria-hidden="true" /></span>
      <h2>No fixture tabs open</h2>
      <Button onClick={() => dock.api && openFixtureList(dock.api)}>Open fixture runs</Button>
    </Dock>}
  />;
}
