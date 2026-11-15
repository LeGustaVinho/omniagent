import type { DockviewApi, SerializedDockview } from "dockview-react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";

function layoutKey({ projectId, checkoutId }: ProjectCheckout) {
  return `omniagent-layout-v1:${projectId}:${checkoutId}:fixtures`;
}

export function restoreFixtureLayout(api: DockviewApi, checkout: ProjectCheckout) {
  const key = layoutKey(checkout);
  try {
    const saved = localStorage.getItem(key);
    if (saved !== null) {
      api.fromJSON(JSON.parse(saved) as SerializedDockview);
      return;
    }
  } catch {
    api.clear();
  }
  openFixtureList(api);
}

export function persistFixtureLayout(api: DockviewApi, checkout: ProjectCheckout) {
  const key = layoutKey(checkout);
  const save = () => {
    try {
      localStorage.setItem(key, JSON.stringify(api.toJSON()));
    } catch {
      // A full or unavailable UI storage still allows layout changes in memory.
    }
  };
  const layout = api.onDidLayoutChange(save);
  const selection = api.onDidActivePanelChange(save);
  window.addEventListener("pagehide", save);
  return () => {
    layout.dispose();
    selection.dispose();
    window.removeEventListener("pagehide", save);
  };
}

export function openFixtureList(api: DockviewApi) {
  const panel = api.getPanel("fixture:list");
  if (panel) return panel.api.setActive();
  api.addPanel({ id: "fixture:list", title: "Fixture runs", component: "fixtures" });
}

export function openFixtureRun(api: DockviewApi, runId: string) {
  const id = `fixture:${runId}`;
  const panel = api.getPanel(id);
  if (panel) return panel.api.setActive();
  api.addPanel({ id, title: `Run ${runId.slice(0, 8)}`, component: "run", params: { runId } });
}
