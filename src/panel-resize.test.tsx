// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { PanelResizeScope, ResizablePanel } from "@/panel-resize";

afterEach(() => {
  cleanup();
  localStorage.clear();
});

it("persists real and simulated panel sizes independently across remounts", () => {
  const realKey = "omniagent:panel-size:IDE navigation:width";
  const simulationKey = "omniagent:panel-size:simulation:IDE navigation:width";
  localStorage.setItem(realKey, "280");

  const panels = <>
    <ResizablePanel as="aside" aria-label="Real Explorer" edge="right" resizeKey="IDE navigation" resizeLabel="IDE navigation" />
    <PanelResizeScope scope="simulation">
      <ResizablePanel as="aside" aria-label="Simulated Explorer" edge="right" resizeKey="IDE navigation" resizeLabel="IDE navigation" />
    </PanelResizeScope>
  </>;
  const width = (panel: HTMLElement) => panel.style.getPropertyValue("--panel-width");
  const view = render(panels);
  const real = screen.getByRole("complementary", { name: "Real Explorer" });
  const simulated = screen.getByRole("complementary", { name: "Simulated Explorer" });
  expect(width(real)).toBe("280px");
  expect(width(simulated)).toBe("");

  vi.spyOn(simulated, "getBoundingClientRect").mockImplementation(() => new DOMRect(0, 0, Number.parseInt(width(simulated), 10) || 248, 400));
  fireEvent.keyDown(within(simulated).getByRole("separator"), { key: "ArrowRight" });
  expect(localStorage.getItem(simulationKey)).toBe("264");
  expect(localStorage.getItem(realKey)).toBe("280");

  view.unmount();
  render(panels);
  expect(width(screen.getByRole("complementary", { name: "Real Explorer" }))).toBe("280px");
  expect(width(screen.getByRole("complementary", { name: "Simulated Explorer" }))).toBe("264px");
});

it("keeps the saved width when the current viewport prevents a resize", () => {
  const key = "omniagent:panel-size:IDE navigation:width";
  localStorage.setItem(key, "440");
  render(<ResizablePanel as="aside" aria-label="Explorer" edge="right" resizeKey="IDE navigation" resizeLabel="IDE navigation" />);
  const panel = screen.getByRole("complementary", { name: "Explorer" });
  const handle = within(panel).getByRole("separator");
  const width = () => panel.style.getPropertyValue("--panel-width");
  vi.spyOn(panel, "getBoundingClientRect").mockImplementation(() => new DOMRect(0, 0, Math.min(400, Number.parseInt(width(), 10)), 400));

  fireEvent.keyDown(handle, { key: "ArrowRight" });
  expect(width()).toBe("440px");
  expect(localStorage.getItem(key)).toBe("440");

  fireEvent.keyDown(handle, { key: "ArrowLeft" });
  expect(width()).toBe("384px");
  expect(localStorage.getItem(key)).toBe("384");
});
