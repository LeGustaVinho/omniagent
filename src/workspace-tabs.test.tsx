// @vitest-environment jsdom
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { DockviewReact, type DockviewApi } from "dockview-react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { openConversation, openConversations, openFixtureRun, persistFixtureLayout, restoreDesktopLayout, restoreFixtureLayout } from "@/workspace-tabs";

const checkout: ProjectCheckout = {
  projectId: "project",
  repositoryRoot: "C:\\repos\\project",
  checkoutId: "checkout",
  checkoutRoot: "C:\\repos\\checkout",
};
const panels = { fixtures: () => null, run: () => null, conversation: () => null, conversations: () => null };

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class {
    observe() {}
    unobserve() {}
    disconnect() {}
  });
  localStorage.clear();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

async function openDock(scope: ProjectCheckout) {
  let api: DockviewApi | undefined;
  let release: (() => void) | undefined;
  const mounted = render(<DockviewReact components={panels} onReady={({ api: view }) => {
    api = view;
    view.layout(960, 540);
    restoreFixtureLayout(view, scope);
    release = persistFixtureLayout(view, scope);
  }} />);
  await waitFor(() => expect(api).toBeDefined());
  return { api: api!, close: () => { release?.(); mounted.unmount(); } };
}

it("restores panel identities, tab order, selection and split groups for the selected checkout", async () => {
  const first = await openDock(checkout);
  act(() => {
    openFixtureRun(first.api, "alpha");
    openFixtureRun(first.api, "bravo");
    const bravo = first.api.getPanel("fixture:bravo")!;
    bravo.api.moveTo({ group: bravo.api.group, position: "center", index: 0 });
    const alpha = first.api.getPanel("fixture:alpha")!;
    alpha.api.moveTo({ group: alpha.api.group, position: "right" });
    bravo.api.setActive();
  });
  expect(first.api.groups).toHaveLength(2);
  const tabOrder = first.api.groups.map((group) => group.panels.map((panel) => panel.id));
  expect(tabOrder).toContainEqual(["fixture:bravo", "fixture:list"]);
  expect(first.api.activePanel?.id).toBe("fixture:bravo");
  window.dispatchEvent(new Event("pagehide"));
  first.close();

  const restored = await openDock(checkout);
  expect(restored.api.groups.map((group) => group.panels.map((panel) => panel.id))).toEqual(tabOrder);
  expect(restored.api.activePanel?.id).toBe("fixture:bravo");
  restored.close();

  const other = await openDock({ ...checkout, checkoutId: "another-worktree" });
  expect(other.api.panels.map((panel) => panel.id)).toEqual(["fixture:list"]);
  other.close();
});

it("keys conversation panels by ID and focuses the existing panel from the list", async () => {
  const dock = await openDock(checkout);
  const alpha = { id: "alpha", title: "Same title", checkoutId: checkout.checkoutId, contextId: "rev-a" };
  const bravo = { ...alpha, id: "bravo", contextId: "rev-b" };
  act(() => {
    openConversations(dock.api);
    openConversation(dock.api, alpha);
    openConversation(dock.api, bravo);
  });
  const first = dock.api.getPanel("conversation:alpha");
  expect(first?.api.renderer).toBe("always");
  expect(dock.api.activePanel?.id).toBe("conversation:bravo");
  act(() => {
    openConversations(dock.api);
    openConversation(dock.api, alpha);
  });
  expect(dock.api.activePanel).toBe(first);
  expect(dock.api.panels.filter((panel) => panel.id.startsWith("conversation:"))).toHaveLength(3);
  act(() => {
    first?.api.close();
    openConversation(dock.api, alpha);
  });
  expect(dock.api.activePanel?.id).toBe("conversation:alpha");
  expect(dock.api.getPanel("conversation:bravo")).toBeDefined();
  dock.close();
});

it("restores each destination from the shared legacy layout while retaining its split tabs", async () => {
  const starting = await openDock(checkout);
  act(() => {
    openConversations(starting.api);
    openConversation(starting.api, { id: "alpha", title: "Alpha", checkoutId: checkout.checkoutId, contextId: "one" });
    openConversation(starting.api, { id: "bravo", title: "Bravo", checkoutId: checkout.checkoutId, contextId: "two" });
    const alpha = starting.api.getPanel("conversation:alpha")!;
    alpha.api.moveTo({ group: alpha.api.group, position: "right" });
    alpha.api.setActive();
  });
  const legacy = JSON.stringify(starting.api.toJSON());
  starting.close();

  const destination = await openDock(checkout);
  act(() => restoreDesktopLayout(destination.api, checkout, "conversations", null, legacy));
  expect(destination.api.groups.map((group) => group.panels.map((panel) => panel.id))).toEqual([
    ["conversation:bravo"], ["conversation:alpha"],
  ]);
  expect(destination.api.activePanel?.id).toBe("conversation:alpha");
  act(() => restoreDesktopLayout(destination.api, checkout, "diagnostics", null, legacy));
  expect(destination.api.panels.map((panel) => panel.id)).toEqual(["fixture:list"]);
  destination.close();
});
