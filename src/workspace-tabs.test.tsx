// @vitest-environment jsdom
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { DockviewReact, type DockviewApi } from "dockview-react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { openFixtureRun, persistFixtureLayout, restoreFixtureLayout } from "@/workspace-tabs";

const checkout: ProjectCheckout = {
  projectId: "project",
  repositoryRoot: "C:\\repos\\project",
  checkoutId: "checkout",
  checkoutRoot: "C:\\repos\\checkout",
};
const panels = { fixtures: () => null, run: () => null };

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
