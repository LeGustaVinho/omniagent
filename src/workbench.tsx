import {
  DockviewReact,
  themeDark,
  type DockviewApi,
  type DockviewReadyEvent,
  type IDockviewHeaderActionsProps,
} from "dockview-react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { Button } from "@/components/ui/button";
import { FixtureListPanel, FixtureRunPanel } from "@/fixture-panels";
import { useRunSubscription } from "@/run-projection";
import { openFixtureList, persistFixtureLayout, restoreFixtureLayout } from "@/workspace-tabs";
import "dockview/dist/styles/dockview.css";

const panels = { fixtures: FixtureListPanel, run: FixtureRunPanel };

function TabActions({ activePanel, panels, group }: IDockviewHeaderActionsProps) {
  if (!activePanel || panels.length < 2) return null;
  const index = panels.indexOf(activePanel);
  return (
    <div className="flex items-center gap-1 px-1">
      <Button className="px-2 py-0 text-xs" aria-label="Move tab left" disabled={index <= 0} variant="outline"
        onClick={() => activePanel.api.moveTo({ group, position: "center", index: index - 1 })}>←</Button>
      <Button className="px-2 py-0 text-xs" aria-label="Move tab right" disabled={index < 0 || index >= panels.length - 1} variant="outline"
        onClick={() => activePanel.api.moveTo({ group, position: "center", index: index + 1 })}>→</Button>
      <Button className="px-2 py-0 text-xs" variant="outline"
        onClick={() => activePanel.api.moveTo({ group, position: "right" })}>Split right</Button>
    </div>
  );
}

export default function Workbench({ checkout }: { checkout: ProjectCheckout }) {
  const [view, setView] = useState<DockviewApi>();
  const restored = useRef<DockviewApi>(null);
  const [layoutReady, setLayoutReady] = useState(false);
  const [layoutError, setLayoutError] = useState("");
  const { error, reconnect } = useRunSubscription(checkout);
  const onReady = useCallback(({ api }: DockviewReadyEvent) => {
    setView(api);
  }, []);
  useEffect(() => {
    if (!view) return;
    let disposed = false;
    let release: (() => void) | undefined;
    let pending: string | undefined;
    let writing = false;
    const scope = { checkoutId: checkout.checkoutId, workspace: "fixtures" };

    async function save(layout: string) {
      pending = layout;
      if (writing) return;
      writing = true;
      try {
        while (pending !== undefined) {
          const next = pending;
          pending = undefined;
          await invoke("save_workbench_layout", { ...scope, layout: next });
        }
        if (!disposed) setLayoutError("");
      } catch (error) {
        if (!disposed) setLayoutError(String(error));
      } finally {
        writing = false;
      }
    }

    void invoke<string | null>("workbench_layout", scope).catch((error: unknown) => {
      if (!disposed) setLayoutError(String(error));
      return null;
    }).then((saved) => {
      if (disposed) return;
      if (restored.current !== view) {
        restoreFixtureLayout(view, checkout, saved);
        restored.current = view;
      }
      release = persistFixtureLayout(view, checkout, (layout) => { void save(layout); });
      setLayoutReady(true);
    });
    return () => {
      disposed = true;
      release?.();
    };
  }, [checkout, view]);

  return (
    <div className="flex min-h-0 flex-1">
      <aside aria-label="Workspace" className="w-35 shrink-0 border-r border-border bg-card p-2.5">
        <Button className="w-full justify-start" variant="outline" disabled={!layoutReady} onClick={() => { if (view) openFixtureList(view); }}>
          Fixture runs
        </Button>
      </aside>
      <section aria-label="Workspace tabs" className="flex min-w-0 flex-1 flex-col">
        {layoutError && <p role="status" className="px-4 py-2 text-sm text-muted-foreground">Layout storage: {layoutError}</p>}
        {error && (
          <div role="alert" className="flex items-center justify-between gap-3 border-b border-border px-4 py-2 text-sm text-red-300">
            <span>{error}</span>
            <Button variant="outline" onClick={reconnect}>Reconnect</Button>
          </div>
        )}
        <div className="workspace-dock min-h-0 flex-1">
          <DockviewReact components={panels} onReady={onReady} theme={themeDark} disableFloatingGroups
            rightHeaderActionsComponent={TabActions} />
        </div>
      </section>
    </div>
  );
}
import { invoke } from "@tauri-apps/api/core";
