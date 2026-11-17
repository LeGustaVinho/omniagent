// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { NoteReading } from "@/knowledge-reading";

afterEach(cleanup);

it("renders note syntax on the same source while keeping comments and active HTML out of Reading", () => {
  const source = ["---", "status: ready", "---", "# Release note", "",
    "#publicação/revisão, #2026, `#inside-code` and ==approved==.", "",
    "> [!TIP] Handoff", "> Confirm the package.", "",
    "| Platform | Status |", "| --- | --- |", "| Windows | ~~queued~~ |", "",
    "- [x] Review artifacts", "", "Equation $x^2$.[^release]", "", "[^release]: Frozen source.", "",
    "```js", "const flag = '#inside-fence'", "```", "",
    "```ps1", 'Write-Output "ready"', "```", "", "%% source-only comment %%", "",
    '<script>window.rawMarkup = true</script>', ""].join("\n");
  const { container } = render(<NoteReading path="Release.md" content={source} files={[]} onLink={vi.fn()} onAttachment={vi.fn()} />);

  expect(screen.getByRole("heading", { name: "Release note" }).getAttribute("data-note-anchor")).toBe("release-note");
  expect(screen.getByRole("term").textContent).toBe("status");
  expect(screen.getByText("ready").tagName).toBe("DD");
  expect([...container.querySelectorAll(".note-tag")].map((tag) => tag.textContent)).toEqual(["#publicação/revisão"]);
  expect(screen.getByText("approved").tagName).toBe("MARK");
  expect(screen.getByText("TIP · Handoff").closest("blockquote")).not.toBeNull();
  expect(within(screen.getByRole("table")).getByText("queued").tagName).toBe("DEL");
  expect((screen.getByRole("checkbox") as HTMLInputElement).checked).toBe(true);
  expect(screen.getByText("Frozen source.").closest("section")?.hasAttribute("data-footnotes")).toBe(true);
  const reference = screen.getByRole("link", { name: "1" });
  const footnote = document.getElementById(reference.getAttribute("href")!.slice(1));
  expect(footnote?.textContent).toContain("Frozen source.");
  expect(within(footnote!).getByRole("link").getAttribute("href")).toBe("#" + reference.id);
  expect(container.querySelector(".katex")).not.toBeNull();
  expect(container.querySelector("pre > code.language-js.hljs .hljs-string")?.textContent).toBe("'#inside-fence'");
  expect(container.querySelector("pre > code.language-ps1.hljs .hljs-built_in")?.textContent).toBe("Write-Output");
  expect(container.textContent).not.toContain("source-only comment");
  expect(container.querySelector("script")).toBeNull();
});

it("maps a source-line jump after frontmatter and an expanded embed to the rendered paragraph", () => {
  const source = "---\r\nstatus: ready\r\n---\r\n# Start\r\n\r\n![[Details]]\r\n\r\nJump here.\r\n\r\nAfter.";
  const { container } = render(<NoteReading path="Release.md" content={source}
    files={[{ path: "Details.md", content: "# Details\n\nEmbedded paragraph." }]}
    jump={{ line: 8, token: 1 }} onLink={vi.fn()} onAttachment={vi.fn()} />);

  expect(container.querySelector("[data-note-line-target]")?.nextElementSibling).toBe(screen.getByText("Jump here."));
});

it("keeps embedded links and rendered selections tied to their original note and source line", () => {
  const source = "---\nstatus: ready\n---\n# Launch\n\n![[ops/Checks#Verify]]\n\nAfter the embed.";
  const embedded = "---\nstatus: draft\n---\n# Preparation\n\n## Verify\n\nRead the checklist.\nSee [the handoff](./handoff%20%231.md#Ready).\n";
  const files = [{ path: "ops/Checks.md", content: embedded }, { path: "ops/handoff #1.md", content: "# Ready" }];
  const chosen = vi.fn();
  render(<NoteReading path="Launch.md" content={source} files={files} onLink={vi.fn()} onAttachment={vi.fn()}
    contextActions={(hit) => [[{ label: "Use reading context", run: () => chosen(hit) }]]} />);
  fireEvent.contextMenu(screen.getByRole("button", { name: "the handoff" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Use reading context" }));
  expect(chosen).toHaveBeenLastCalledWith(expect.objectContaining({ link: expect.objectContaining({
    raw: "./handoff%20%231.md#Ready", target: "./handoff #1.md", anchor: "Ready", sourcePath: "ops/Checks.md", sourceLine: 9, syntax: "markdown",
  }) }));

  const paragraph = screen.getByText("After the embed.");
  const selection = document.getSelection()!;
  const range = document.createRange();
  range.setStart(paragraph.firstChild!, 0);
  range.setEnd(paragraph.firstChild!, 5);
  selection.removeAllRanges();
  selection.addRange(range);
  fireEvent.contextMenu(paragraph);
  fireEvent.click(screen.getByRole("menuitem", { name: "Use reading context" }));
  expect(chosen).toHaveBeenLastCalledWith(expect.objectContaining({ excerpt: {
    path: "Launch.md", text: "After", startLine: 8, endLine: 8, language: "markdown",
  } }));
  selection.removeAllRanges();
});
