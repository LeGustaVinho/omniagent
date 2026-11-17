// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  KnowledgeLinkPreview, KnowledgeLinks, KnowledgeOutline, KnowledgeSearch,
  type KnowledgeOutgoingLink, type KnowledgeSearchHit,
} from "@/knowledge-navigation";

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class {
    observe() {}
    unobserve() {}
    disconnect() {}
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const first = {
  path: "planos/Revisão.md", line: 7, anchor: "Preparation", title: "Revisão", snippet: "The release notes include the deployment window.",
} satisfies KnowledgeSearchHit;
const second = {
  path: "arquivo/Revisão.md", line: 26, anchor: "^release-block", title: "Revisão", snippet: "The archived release notes describe the rollback.",
} satisfies KnowledgeSearchHit;

it("quick-opens the selected canonical search location by keyboard and mouse", async () => {
  const open = vi.fn();
  function Fixture() {
    const [query, change] = useState("");
    return <KnowledgeSearch query={query} onQueryChange={change} results={query ? [first, second] : []}
      onOpen={open} focusRequest={1} />;
  }
  render(<Fixture />);
  const user = userEvent.setup();
  const input = screen.getByRole("combobox", { name: "Find notes and content" });
  expect(document.activeElement).toBe(input);
  await user.type(input, "release notes");

  const list = screen.getByRole("listbox", { name: "Note search results" });
  const rows = within(list).getAllByRole("option");
  expect(within(rows[0]!).getByText("planos/Revisão.md#Preparation · Line 7")).toBeTruthy();
  expect(within(rows[1]!).getByText(second.snippet)).toBeTruthy();
  expect(input.getAttribute("aria-activedescendant")).toBe(rows[0]!.id);
  await user.keyboard("{ArrowDown}");
  expect(input.getAttribute("aria-activedescendant")).toBe(rows[1]!.id);
  expect(rows[1]!.getAttribute("aria-selected")).toBe("true");
  await user.keyboard("{Enter}");
  expect(open).toHaveBeenCalledExactlyOnceWith({ path: second.path, line: second.line, anchor: second.anchor });
  expect(screen.queryByRole("listbox")).toBeNull();

  await user.keyboard("{ArrowDown}{Escape}{Enter}");
  expect(open).toHaveBeenCalledTimes(1);
  await user.keyboard("{ArrowDown}");
  await user.click(screen.getAllByRole("option")[0]!);
  expect(open).toHaveBeenLastCalledWith({ path: first.path, line: first.line, anchor: first.anchor });
});

it("keeps results from an in-flight query out of quick opening", async () => {
  const open = vi.fn();
  const change = vi.fn();
  const { rerender } = render(<KnowledgeSearch query="heading" onQueryChange={change} results={[first]} onOpen={open} focusRequest={1} pending />);
  const user = userEvent.setup();
  expect(screen.getByRole("status").textContent).toBe("Searching notes…");
  await user.keyboard("{ArrowDown}{Enter}");
  expect(open).not.toHaveBeenCalled();
  expect(screen.queryByRole("listbox")).toBeNull();

  rerender(<KnowledgeSearch query="heading" onQueryChange={change} results={[]} onOpen={open} />);
  expect(screen.getByRole("status").textContent).toBe("No notes match your search.");
  rerender(<KnowledgeSearch query="heading" onQueryChange={change} results={[]} onOpen={open} error="The index could not be read." />);
  expect(screen.getByRole("alert").textContent).toBe("The index could not be read.");
});

it("retains a keyboard selection when the index returns the same results in a new order", async () => {
  const open = vi.fn();
  const change = vi.fn();
  const { rerender } = render(<KnowledgeSearch query="release" onQueryChange={change} results={[first, second]} onOpen={open} focusRequest={1} />);
  const user = userEvent.setup();
  await user.keyboard("{ArrowDown}");
  rerender(<KnowledgeSearch query="release" onQueryChange={change} results={[second, first]} onOpen={open} focusRequest={1} />);
  await user.keyboard("{Enter}");
  expect(open).toHaveBeenCalledExactlyOnceWith({ path: second.path, line: second.line, anchor: second.anchor });
});

it("previews and opens exact outline, outgoing and backlink locations and routes unresolved links to repair", async () => {
  const open = vi.fn();
  const repair = vi.fn();
  const source = { path: "notes/Start.md", line: 13, anchor: null };
  const resolved: KnowledgeOutgoingLink = { label: "Deployment window", rawTarget: "Plan#Preparation", source,
    resolution: { kind: "resolved", target: first } };
  const missing: KnowledgeOutgoingLink = { label: "Retired notes", rawTarget: "Removed", source: { ...source, line: 14 },
    resolution: { kind: "missing" } };
  const ambiguous: KnowledgeOutgoingLink = { label: "Review", rawTarget: "Revisão", source: { ...source, line: 15 },
    resolution: { kind: "ambiguous", candidates: [first, second] } };
  render(<>
    <KnowledgeOutline entries={[{ ...source, line: 3, anchor: "Overview", label: "Overview", level: 2 }]} onOpen={open} />
    <KnowledgeLinks outgoing={[resolved, missing, ambiguous]}
      backlinks={[{ source: { ...second, line: 31, anchor: null }, label: "Archived reference" }]} onOpen={open} onRepair={repair} />
  </>);

  const user = userEvent.setup();
  await user.click(within(screen.getByRole("region", { name: "Outline" })).getByRole("button", { name: "Overview" }));
  expect(open).toHaveBeenLastCalledWith({ path: source.path, line: 3, anchor: "Overview" });

  const outgoing = within(screen.getByRole("region", { name: "Outgoing links" }));
  const target = outgoing.getByRole("button", { name: "Deployment window" });
  await user.hover(target);
  const preview = await screen.findByRole("tooltip");
  expect(preview.textContent).toContain("planos/Revisão.md#Preparation · Line 7");
  expect(preview.textContent).toContain(first.snippet);
  await user.click(target);
  expect(open).toHaveBeenLastCalledWith({ path: first.path, line: first.line, anchor: first.anchor });

  await user.click(outgoing.getByRole("button", { name: "Retired notes · Missing" }));
  await user.click(outgoing.getByRole("button", { name: "Review · Ambiguous" }));
  expect(repair.mock.calls).toEqual([[missing], [ambiguous]]);
  expect(open).toHaveBeenCalledTimes(2);
  await user.click(outgoing.getByText("Candidate notes"));
  await user.click(outgoing.getByRole("button", { name: second.path }));
  expect(open).toHaveBeenLastCalledWith({ path: second.path, line: second.line, anchor: second.anchor });

  await user.click(within(screen.getByRole("region", { name: "Backlinks" })).getByRole("button", { name: "Archived reference" }));
  expect(open).toHaveBeenLastCalledWith({ path: second.path, line: 31, anchor: null });
});

it("opens the exact source of an unresolved inline link when repair is unavailable", async () => {
  const open = vi.fn();
  const source = { path: "notes/Start.md", line: 18, anchor: "^ref" };
  const link: KnowledgeOutgoingLink = { label: "Broken", source, rawTarget: "Not indexed", resolution: { kind: "missing" } };
  render(<KnowledgeLinkPreview link={link} onOpen={open} />);
  await userEvent.setup().click(screen.getByRole("button", { name: "Broken · Missing" }));
  expect(open).toHaveBeenCalledExactlyOnceWith(source);
});

it("opens the exact reference location and repair from the link menu", async () => {
  const open = vi.fn();
  const repair = vi.fn();
  const source = { path: "notes/Start.md", line: 18, anchor: "^ref" };
  const link: KnowledgeOutgoingLink = { label: "Broken", source, rawTarget: "Not indexed", resolution: { kind: "missing" } };
  render(<KnowledgeLinks outgoing={[link]} backlinks={[]} onOpen={open} onRepair={repair} actions={() => []} />);
  const user = userEvent.setup();
  const row = screen.getByRole("button", { name: "Broken · Missing" });
  fireEvent.contextMenu(row);
  await user.click(await screen.findByRole("menuitem", { name: "Go to link in source" }));
  expect(open).toHaveBeenCalledExactlyOnceWith(source);
  fireEvent.contextMenu(row);
  await user.click(await screen.findByRole("menuitem", { name: "Repair link…" }));
  expect(repair).toHaveBeenCalledExactlyOnceWith(link);
});
