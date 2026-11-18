// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { PanelResizeScope, ResizablePanel } from "@/panel-resize";

afterEach(() => {
  cleanup();
  localStorage.clear();
});

it("persists workspace and changes inspector sizes independently across remounts", () => {
  const realKey = "omniagent:panel-size:IDE navigation:width";
  const inspectorKey = "omniagent:panel-size:changes-inspector:IDE navigation:width";
  localStorage.setItem(realKey, "280");

  const panels = <>
    <ResizablePanel as="aside" aria-label="Workspace Explorer" edge="right" resizeKey="IDE navigation" resizeLabel="IDE navigation" />
    <PanelResizeScope scope="changes-inspector">
      <ResizablePanel as="aside" aria-label="Changes Explorer" edge="right" resizeKey="IDE navigation" resizeLabel="IDE navigation" />
    </PanelResizeScope>
  </>;
  const width = (panel: HTMLElement) => panel.style.getPropertyValue("--panel-width");
  const view = render(panels);
  const real = screen.getByRole("complementary", { name: "Workspace Explorer" });
  const inspector = screen.getByRole("complementary", { name: "Changes Explorer" });
  expect(width(real)).toBe("280px");
  expect(width(inspector)).toBe("");

  vi.spyOn(inspector, "getBoundingClientRect").mockImplementation(() => new DOMRect(0, 0, Number.parseInt(width(inspector), 10) || 248, 400));
  fireEvent.keyDown(within(inspector).getByRole("separator"), { key: "ArrowRight" });
  expect(localStorage.getItem(inspectorKey)).toBe("264");
  expect(localStorage.getItem(realKey)).toBe("280");

  view.unmount();
  render(panels);
  expect(width(screen.getByRole("complementary", { name: "Workspace Explorer" }))).toBe("280px");
  expect(width(screen.getByRole("complementary", { name: "Changes Explorer" }))).toBe("264px");
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
