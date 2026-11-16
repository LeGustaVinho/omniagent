// @vitest-environment jsdom

import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { EditorView } from "@codemirror/view";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { renderNoteDiagram } from "@/knowledge-diagram";
import { MarkdownEditor } from "@/knowledge-editor";
import { NoteReading } from "@/knowledge-reading";
import referenceNote from "../tests/fixtures/obsidian-reference/reference.md?raw";
import targetNote from "../tests/fixtures/obsidian-reference/details.md?raw";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/knowledge-diagram", () => ({ renderNoteDiagram: vi.fn() }));

const rangeLayout = { getClientRects: () => [], getBoundingClientRect: () => new DOMRect() };
const missingLayoutMethods = Object.keys(rangeLayout).filter((name) => !(name in Range.prototype)) as (keyof typeof rangeLayout)[];
beforeEach(() => {
  for (const name of missingLayoutMethods) {
    Object.defineProperty(Range.prototype, name, { configurable: true, value: rangeLayout[name] });
  }
});
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
  for (const name of missingLayoutMethods) Reflect.deleteProperty(Range.prototype, name);
});

const path = "notes/reference.md";
const targetPath = "notes/details.md";
const link = "[[./details#Evidence|open the reference]]";
const strong = "**the fixture**";
const diagram = "flowchart LR\n  A[Draft] --> B[Review]\n";

// The Rust obsidian_reference integration test checks these Comrak spans
// against the same source in a repository checkout.
function canonicalResponse(markdown: string, bufferRevision: string) {
  function range(snippet: string, offset = 0, length = snippet.length) {
    const start = markdown.indexOf(snippet);
    expect(start).toBeGreaterThanOrEqual(0);
    return { from: start + offset, to: start + offset + length };
  }
  const task = markdown.includes("[x] Review the table") ? "[x]" : "[ ]";
  const taskLine = `${task} Review the table`;
  return { bufferRevision, nodes: [
    { kind: "wikilink", range: range(link), target: "./details", anchor: "Evidence", alias: "open the reference" },
    { kind: "strong", range: range(strong), markers: [range(strong, 0, 2), range(strong, strong.length - 2, 2)] },
    { kind: "task_marker", range: range(taskLine, 0, 3), status: task[1], checked: task[1] === "x", valueRange: range(taskLine, 1, 1) },
  ] };
}

it("compares independently written Obsidian syntax across Source, Live Preview and Reading in a Git checkout", { timeout: 20_000 }, async () => {
  const temp = realpathSync(tmpdir());
  const checkout = mkdtempSync(join(temp, "omniagent-obsidian-reference-"));
  try {
    const git = (...args: string[]) => execFileSync("git", args, { cwd: checkout, encoding: "utf8", windowsHide: true });
    git("init", "--quiet");
    git("config", "core.autocrlf", "false");
    mkdirSync(join(checkout, "notes"));
    writeFileSync(join(checkout, path), referenceNote.replaceAll("\r\n", "\n"));
    writeFileSync(join(checkout, targetPath), targetNote.replaceAll("\r\n", "\n"));
    git("add", "--", path, targetPath);
    git("-c", "user.name=Compatibility fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "Add independent examples");
    const source = readFileSync(join(checkout, path), "utf8");
    const target = readFileSync(join(checkout, targetPath), "utf8");
    const onChange = vi.fn();
    vi.mocked(invoke).mockImplementation(async (command, parameters) => {
      expect(command).toBe("analyze_note_markdown");
      const { markdown, bufferRevision } = parameters as { markdown: string; bufferRevision: string };
      return canonicalResponse(markdown, bufferRevision);
    });
    vi.mocked(renderNoteDiagram).mockResolvedValue("data:image/svg+xml," + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg"/>'));

    function Note() {
      const [content, setContent] = useState(source);
      const [mode, setMode] = useState<"source" | "live">("source");
      const files = [{ path, content }, { path: targetPath, content: target }];
      return <>
        <button type="button" onClick={() => setMode("live")}>Live Preview</button>
        <MarkdownEditor path={path} source={content} mode={mode} files={files} analyze onLink={() => {}}
          onChange={(next) => { onChange(next); setContent(next); }} />
        <NoteReading path={path} content={content} files={files} onLink={() => {}} onAttachment={() => {}} sampleAttachments={false} />
      </>;
    }

    const { container } = render(<Note />);
    const view = EditorView.findFromDOM(container.querySelector<HTMLElement>(".cm-editor")!)!;
    const reading = within(screen.getByLabelText(`Reading view for ${path}`));
    expect(view.state.doc.toString()).toBe(source);
    expect(view.contentDOM.textContent).toContain(link);
    expect(view.contentDOM.textContent).toContain(strong);
    expect(onChange).not.toHaveBeenCalled();
    expect(git("status", "--short")).toBe("");

    const cell = source.indexOf("Pending");
    act(() => view.dispatch({ changes: { from: cell, to: cell + "Pending".length, insert: "Ready" } }));
    expect(reading.getByRole("cell", { name: "Ready" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Live Preview" }));
    const checkbox = await screen.findByRole<HTMLInputElement>("checkbox", { name: "Toggle note task" });
    expect(view.contentDOM.textContent).not.toContain(link);
    expect(view.contentDOM.textContent).not.toContain(strong);
    expect(view.contentDOM.querySelector(".cm-note-link")?.textContent).toBe("open the reference");

    act(() => view.dispatch({ selection: { anchor: source.indexOf(link) + 4 } }));
    expect(view.contentDOM.textContent).toContain(link);
    act(() => view.dispatch({ selection: { anchor: source.indexOf(strong) + 4 } }));
    expect(view.contentDOM.textContent).toContain(strong);
    act(() => view.dispatch({ selection: { anchor: 0 } }));
    expect(view.contentDOM.textContent).not.toContain(strong);

    fireEvent.click(checkbox);
    expect(reading.getByRole<HTMLInputElement>("checkbox").checked).toBe(true);
    expect(reading.getByText(/Keep the custom state/)).toBeTruthy();
    expect(reading.getByText("NOTE · Calibration")).toBeTruthy();
    const rendered = screen.getByLabelText(`Reading view for ${path}`);
    expect(rendered.querySelectorAll(".katex")).toHaveLength(2);
    expect(rendered.querySelectorAll(".katex-display")).toHaveLength(1);
    const embed = within(reading.getByText("Embedded · details.md").closest("blockquote")!);
    expect(embed.getByRole("heading", { name: "Evidence" })).toBeTruthy();
    expect(embed.getByText(/The test reading is within tolerance/)).toBeTruthy();
    expect(embed.queryByText(/outside the (selected section|embed)/)).toBeNull();
    const footnote = rendered.querySelector<HTMLAnchorElement>('sup a[href$="-fn-tolerance"]');
    expect(footnote?.textContent).toBe("1");
    expect(document.getElementById(decodeURIComponent(footnote!.hash.slice(1)))?.textContent).toContain("Measured on this fixture.");
    expect(await reading.findByRole("img", { name: "Mermaid diagram from the note" })).toBeTruthy();
    expect(renderNoteDiagram).toHaveBeenCalledWith(diagram, expect.any(AbortSignal));

    const expected = source.replace("| Start | Pending |", "| Start | Ready |").replace("[ ] Review", "[x] Review");
    await waitFor(() => expect(view.state.doc.toString()).toBe(expected));
    expect(onChange).toHaveBeenCalledTimes(2);
    writeFileSync(join(checkout, path), view.state.doc.toString());
    expect(git("diff", "--name-only").trim()).toBe(path);
    expect(git("diff", "--numstat").trim()).toBe(`2\t2\t${path}`);
    expect(readFileSync(join(checkout, targetPath), "utf8")).toBe(target);
  } finally {
    if (dirname(realpathSync(checkout)) !== temp) throw new Error("Invalid reference checkout cleanup path");
    rmSync(checkout, { recursive: true, force: true });
  }
});
