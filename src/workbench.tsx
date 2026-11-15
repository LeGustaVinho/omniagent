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
  const { error, reconnect } = useRunSubscription(checkout);
  const onReady = useCallback(({ api }: DockviewReadyEvent) => {
    if (restored.current !== api) {
      restoreFixtureLayout(api, checkout);
      restored.current = api;
    }
    setView(api);
  }, [checkout]);
  useEffect(() => {
    if (view) return persistFixtureLayout(view, checkout);
  }, [checkout, view]);

  return (
    <div className="flex min-h-0 flex-1">
      <aside aria-label="Workspace" className="w-35 shrink-0 border-r border-border bg-card p-2.5">
        <Button className="w-full justify-start" variant="outline" onClick={() => { if (view) openFixtureList(view); }}>
          Fixture runs
        </Button>
      </aside>
      <section aria-label="Workspace tabs" className="flex min-w-0 flex-1 flex-col">
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
