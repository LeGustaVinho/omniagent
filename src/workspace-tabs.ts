import type { DockviewApi, SerializedDockview } from "dockview-react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { Conversation } from "@/bindings/Conversation";

function layoutKey({ projectId, checkoutId }: ProjectCheckout, workspace = "fixtures") {
  return `omniagent-layout-v1:${projectId}:${checkoutId}:${workspace}`;
}

export type DesktopDockArea = "conversations" | "diagnostics";

export function restoreDesktopLayout(api: DockviewApi, checkout: ProjectCheckout, area: DesktopDockArea, stored: string | null, legacy: string | null) {
  try {
    const saved = stored ?? localStorage.getItem(layoutKey(checkout, area)) ?? legacy ?? localStorage.getItem(layoutKey(checkout));
    if (saved !== null) {
      api.fromJSON(JSON.parse(saved) as SerializedDockview);
      for (const panel of [...api.panels]) {
        const keep = area === "diagnostics" ? panel.id.startsWith("fixture:") : panel.id.startsWith("conversation:") && panel.id !== "conversation:list";
        if (!keep) api.removePanel(panel);
      }
      if (!api.activePanel) api.panels[0]?.api.setActive();
    }
  } catch {
    api.clear();
  }
  if (area === "diagnostics" && api.panels.length === 0) openFixtureList(api);
}

export function restoreFixtureLayout(api: DockviewApi, checkout: ProjectCheckout, stored?: string | null) {
  const key = layoutKey(checkout);
  try {
    const saved = stored ?? localStorage.getItem(key);
    if (saved !== null) {
      api.fromJSON(JSON.parse(saved) as SerializedDockview);
      return;
    }
  } catch {
    api.clear();
  }
  openFixtureList(api);
}

export function persistFixtureLayout(api: DockviewApi, checkout: ProjectCheckout, onChange?: (serialized: string) => void, workspace = "fixtures") {
  const key = layoutKey(checkout, workspace);
  const save = () => {
    const serialized = JSON.stringify(api.toJSON());
    try {
      localStorage.setItem(key, serialized);
    } catch {
      // A full or unavailable UI storage still allows layout changes in memory.
    }
    onChange?.(serialized);
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

export function openProviderSettings(api: DockviewApi) {
  const panel = api.getPanel("provider-settings");
  if (panel) return panel.api.setActive();
  api.addPanel({ id: "provider-settings", title: "Provider settings", component: "providerSettings" });
}

export function openCustomizations(api: DockviewApi) {
  const panel = api.getPanel("customizations");
  if (panel) return panel.api.setActive();
  api.addPanel({ id: "customizations", title: "Customizations", component: "customizations" });
}

export function openFixtureRun(api: DockviewApi, runId: string) {
  const id = `fixture:${runId}`;
  const panel = api.getPanel(id);
  if (panel) return panel.api.setActive();
  api.addPanel({ id, title: `Run ${runId.slice(0, 8)}`, component: "run", params: { runId } });
}

export function openConversations(api: DockviewApi) {
  const panel = api.getPanel("conversation:list");
  if (panel) return panel.api.setActive();
  api.addPanel({ id: "conversation:list", title: "Conversations", component: "conversations" });
}

export function openConversation(api: DockviewApi, conversation: Conversation) {
  const id = `conversation:${conversation.id}`;
  const panel = api.getPanel(id);
  if (panel) return panel.api.setActive();
  api.addPanel({
    id,
    title: conversation.title,
    component: "conversation",
    renderer: "always",
    params: { conversationId: conversation.id, title: conversation.title },
  });
}
