import { invoke } from "@tauri-apps/api/core";
import {
  DockviewReact,
  themeDark,
  type DockviewApi,
  type DockviewReadyEvent,
  type IDockviewHeaderActionsProps,
} from "dockview-react";
import { ArrowLeft, ArrowRight, Columns2, GitBranch, MessageSquarePlus, PanelLeftClose } from "lucide-react";
import { Dialog } from "radix-ui";
import { useEffect, useState } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { ConversationListPanel, ConversationPanel, ConversationSidebar } from "@/conversation-panels";
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
  dock, children,
}: { dock: ReturnType<typeof useDesktopDock>; children?: React.ReactNode }) {
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      {dock.layoutError && <p role="status" className="work-inline px-4 py-2">Layout storage: {dock.layoutError}</p>}
      <div className="workspace-dock relative min-h-0 flex-1">
        <DockviewReact components={legacyPanels} onReady={dock.onReady} theme={themeDark} disableFloatingGroups rightHeaderActionsComponent={TabActions} />
        {!dock.ready && <div className="work-dock-empty" role="status">Restoring tabs…</div>}
        {dock.ready && !dock.hasPanels && children && <div className="work-dock-empty">{children}</div>}
      </div>
    </div>
  );
}

export function RealConversations({
  checkout, sidebar, onSidebarChange, compact, onSimulate, target,
}: {
  checkout: ProjectCheckout;
  sidebar: boolean;
  onSidebarChange: (visible: boolean) => void;
  compact: boolean;
  onSimulate: () => void;
  target?: string;
}) {
  const dock = useDesktopDock(checkout, "conversations");
  useEffect(() => {
    if (dock.ready && dock.api && target) {
      dock.api.getPanel("conversation:" + target)?.api.setActive();
    }
  }, [dock.api, dock.ready, target]);

  function open(conversation: Conversation) {
    if (!dock.api) return;
    openConversation(dock.api, conversation);
    if (compact) onSidebarChange(false);
  }

  function updateTitle(conversation: Conversation) {
    const panel = dock.api?.getPanel("conversation:" + conversation.id);
    if (!panel) return;
    panel.api.setTitle(conversation.title);
    panel.api.updateParameters({ title: conversation.title });
  }

  const contents = dock.ready
    ? <ConversationSidebar checkoutId={checkout.checkoutId} onOpen={open} onRenamed={updateTitle} activeId={dock.activeId} onSimulate={onSimulate} />
    : <p className="work-progress" role="status">Loading conversations…</p>;
  return (
    <div className="flex h-full min-h-0 min-w-0">
      {!compact && sidebar && <aside className="work-context">{contents}</aside>}
      {compact && <Dialog.Root open={sidebar} onOpenChange={onSidebarChange}>
        <Dialog.Portal>
          <Dialog.Overlay className="work-shade" />
          <Dialog.Content className="work-context work-context-drawer" aria-describedby={undefined}>
            <header className="work-panel-header">
              <Dialog.Title className="font-semibold">Conversation navigation</Dialog.Title>
              <Dialog.Close asChild><Button variant="ghost" aria-label="Close conversation navigation"><PanelLeftClose size={16} /></Button></Dialog.Close>
            </header>
            {contents}
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>}
      <Dock dock={dock}>
        <MessageSquarePlus size={28} />
        <h2>Open a conversation</h2>
        <p>Start a new conversation or return to one from this checkout.</p>
        {!sidebar && <Button variant="outline" onClick={() => onSidebarChange(true)}>Show conversations</Button>}
      </Dock>
    </div>
  );
}

export function RealDiagnostics({ checkout }: { checkout: ProjectCheckout }) {
  const dock = useDesktopDock(checkout, "diagnostics");
  return (
    <section aria-label="Diagnostics workspace" className="flex h-full min-h-0 flex-col">
      <div className="work-panel-header">
        <span className="font-semibold">Diagnostics</span>
        <Button variant="ghost" disabled={!dock.ready} onClick={() => dock.api && openFixtureList(dock.api)}><GitBranch size={15} /> Fixture runs</Button>
      </div>
      <Dock dock={dock} />
    </section>
  );
}
