// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { BookOpen, Code, GitBranch } from "lucide-react";
import { afterEach, expect, it, vi } from "vitest";
import { GoTo } from "@/workbench-navigation";
import { WorkbenchPanel } from "@/workbench-panel";

afterEach(cleanup);

it("focuses destination search, filters by name, and opens the highlighted result with Enter", () => {
  const openKnowledge = vi.fn();
  const openGit = vi.fn();
  const close = vi.fn();
  render(<GoTo onClose={close} destinations={[
    { label: "IDE", icon: Code, select: vi.fn() },
    { label: "Knowledge", icon: BookOpen, select: openKnowledge },
    { label: "Git", icon: GitBranch, select: openGit },
  ]} />);

  const search = screen.getByRole("combobox", { name: "Search destinations" });
  expect(document.activeElement).toBe(search);
  fireEvent.change(search, { target: { value: "g" } });
  expect(screen.queryByRole("option", { name: "IDE" })).toBeNull();
  fireEvent.keyDown(search, { key: "ArrowDown" });
  expect(search.getAttribute("aria-activedescendant")).toBe(screen.getByRole("option", { name: "Git" }).id);
  fireEvent.keyDown(search, { key: "Enter" });

  expect(openGit).toHaveBeenCalledOnce();
  expect(openKnowledge).not.toHaveBeenCalled();
  expect(close).toHaveBeenCalledOnce();
});

it("keeps a panel's field and its value when switching from docked to compact and back", async () => {
  const onClose = vi.fn();
  const panel = (drawer: boolean) => <WorkbenchPanel label="Queue" drawer={drawer} open
    className="work-queue-panel" drawerClass="work-queue-drawer" edge="top" resizeKey="test-queue" resizeLabel="queue" onClose={onClose}>
    <label>Instructions<input defaultValue="" /></label>
  </WorkbenchPanel>;
  const { rerender } = render(panel(false));
  const field = screen.getByRole<HTMLInputElement>("textbox", { name: "Instructions" });
  fireEvent.input(field, { target: { value: "Keep this draft" } });

  rerender(panel(true));
  expect((await screen.findByRole("dialog", { name: "Queue" })).contains(field)).toBe(true);
  expect(field.value).toBe("Keep this draft");
  rerender(panel(false));
  expect(screen.queryByRole("dialog", { name: "Queue" })).toBeNull();
  expect(screen.getByRole("textbox", { name: "Instructions" })).toBe(field);
  expect(field.value).toBe("Keep this draft");
});
