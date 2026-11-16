// @vitest-environment jsdom

import { EditorView } from "@codemirror/view";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { createElement, Fragment, useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { MarkdownEditor } from "@/knowledge-editor";
import { NoteReading } from "@/knowledge-reading";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const utf8 = (source: string) => new TextEncoder().encode(source);

function mountNote(path: string, original: string, mode: "source" | "live" = "source") {
  const onChange = vi.fn<(source: string) => void>();
  const onLink = vi.fn();
  const onAttachment = vi.fn();
  let buffer = original;
  function Note() {
    const [source, setSource] = useState(original);
    const files = [{ path, content: source }];
    return createElement(Fragment, null,
      createElement(MarkdownEditor, { path, source, files, mode, analyze: mode === "live", onLink, onChange(next) {
        buffer = next;
        onChange(next);
        setSource(next);
      } }),
      createElement(NoteReading, { path, content: source, files, onLink, onAttachment, sampleAttachments: false }),
    );
  }

  const { container } = render(createElement(Note));
  const reading = screen.getByLabelText(`Reading view for ${path}`);
  const editor = EditorView.findFromDOM(container.querySelector<HTMLElement>(".cm-editor")!)!;
  function replaceSourceText(before: string, after: string) {
    const start = editor.state.doc.toString().indexOf(before);
    expect(start).toBeGreaterThanOrEqual(0);
    act(() => editor.dispatch({ changes: { from: start, to: start + before.length, insert: after } }));
  }
  return { editor, reading, onChange, replaceSourceText, buffer: () => buffer };
}

it("reads Dataview fields and query source and edits only a neighboring GFM table cell", () => {
  const fields = "Reviewer:: Joana\n\nReview result (status:: pending).";
  const query = "TABLE file.link, Reviewer\nFROM #project/release\nWHERE status != \"complete\"\nSORT file.mtime DESC";
  const source = `# Release tracker\n\n${fields}\n\n\`\`\`dataview\n${query}\n\`\`\`\n\n| Round | Outcome |\n| --- | --- |\n| Pilot | Draft |\n`;
  const note = mountNote("notes/release-tracker.md", source);
  expect(within(note.reading).getByText("Reviewer:: Joana")).toBeTruthy();
  expect(within(note.reading).getByText("Review result (status:: pending).")).toBeTruthy();
  expect(note.reading.querySelector("code.language-dataview")?.textContent).toBe(query + "\n");
  expect(note.editor.state.doc.toString()).toBe(source);
  expect(note.onChange).not.toHaveBeenCalled();
  expect(utf8(note.buffer())).toEqual(utf8(source));

  note.replaceSourceText("| Pilot | Draft |", "| Pilot | Ready |");
  expect(within(note.reading).getByRole("cell", { name: "Ready" })).toBeTruthy();
  expect(note.reading.querySelector("code.language-dataview")?.textContent).toBe(query + "\n");
  expect(utf8(note.buffer())).toEqual(utf8(source.replace("| Pilot | Draft |", "| Pilot | Ready |")));
});

it("toggles a standard checkbox beside a custom Tasks state without altering metadata", async () => {
  const waiting = "Ship a patch ⏫ 🔁 every week 🛫 2026-11-25 📅 2026-11-27 #release/docs";
  const custom = "[/] Localize captions 👤 Ana 🆔 message-check ⛔ subtitle-review";
  const source = `# Release chores\n\n- [ ] ${waiting}\n- ${custom}\n`;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    expect(command).toBe("analyze_note_markdown");
    const { markdown, bufferRevision } = args as { markdown: string; bufferRevision: string };
    const nodes = Array.from(markdown.matchAll(/^- \[([ x/])\]/gm), ({ index, 1: status }) => ({
      kind: "task_marker", status, checked: status === "x", range: { from: index + 2, to: index + 5 },
      valueRange: { from: index + 3, to: index + 4 },
    }));
    return { bufferRevision, nodes };
  });
  const note = mountNote("notes/release-chores.md", source, "live");
  const checkbox = await screen.findByRole<HTMLInputElement>("checkbox", { name: "Toggle note task" });
  const renderedCheckbox = within(note.reading).getByRole<HTMLInputElement>("checkbox");
  expect(renderedCheckbox.checked).toBe(false);
  expect(renderedCheckbox.disabled).toBe(true);
  expect(within(note.reading).getByText(custom)).toBeTruthy();
  expect(note.onChange).not.toHaveBeenCalled();
  expect(utf8(note.buffer())).toEqual(utf8(source));

  fireEvent.click(checkbox);
  expect(within(note.reading).getByRole<HTMLInputElement>("checkbox").checked).toBe(true);
  expect(within(note.reading).getByText(waiting)).toBeTruthy();
  expect(within(note.reading).getByText(custom)).toBeTruthy();
  expect(note.onChange).toHaveBeenCalledOnce();
  expect(utf8(note.buffer())).toEqual(utf8(source.replace("- [ ] Ship", "- [x] Ship")));
});

it("reads Kanban columns as Markdown and preserves its YAML and hidden settings through a list edit", () => {
  const frontmatter = "\uFEFF---\nkanban-plugin: board # keep the board type\nunknown-key: {columns: ['Later', 'Doing']}\n---\n";
  const settings = '%% kanban:settings\n```\n{"kanban-plugin":"board","list-collapse":[false,true]}\n```\n%%';
  const source = `${frontmatter}\n## Later\n\n- [ ] Draft brief\n\n## Doing\n\n- [ ] Pair on release\n\n${settings}\n`;
  const note = mountNote("notes/release-board.md", source);
  expect(within(note.reading).getByRole("heading", { name: "Later" })).toBeTruthy();
  expect(within(note.reading).getByRole("heading", { name: "Doing" })).toBeTruthy();
  expect(within(note.reading).getByText("board")).toBeTruthy();
  expect(note.reading.textContent).not.toContain("list-collapse");
  expect(note.editor.state.doc.toString()).toBe(source);
  expect(note.onChange).not.toHaveBeenCalled();
  expect(utf8(note.buffer())).toEqual(utf8(source));

  note.replaceSourceText("- [ ] Draft brief", "- [ ] Publish brief");
  expect(within(note.reading).getByText("Publish brief")).toBeTruthy();
  expect(note.reading.textContent).not.toContain("list-collapse");
  expect(utf8(note.buffer())).toEqual(utf8(source.replace("- [ ] Draft brief", "- [ ] Publish brief")));
});
