// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { parseReadingAddress, parseWiki, prepareReading, readingLocalAddress, resolveWiki } from "@/knowledge-document";
import { NoteReading } from "@/knowledge-reading";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("resolves checkout-root, note-relative, and explicit paths without escaping the catalog", () => {
  const path = "docs/guides/start.md";
  const files = [
    { path: "manual/Runbook.md", content: "# Root" },
    { path: "docs/guides/manual/Runbook.md", content: "# Alongside the note" },
    { path: "docs/Overview.markdown", content: "# Parent folder" },
    { path: "docs/guides/runbooks/Details.md", content: "# Relative fallback" },
  ];
  const resolve = (target: string) => resolveWiki(parseWiki(target), path, files);
  expect(resolve("/manual/Runbook#Approval|Review"))
    .toEqual([{ path: "manual/Runbook.md", anchor: "Approval" }]);
  expect(resolve("manual/Runbook")).toEqual([{ path: "manual/Runbook.md", anchor: undefined }]);
  expect(resolve("./manual/Runbook")).toEqual([{ path: "docs/guides/manual/Runbook.md", anchor: undefined }]);
  expect(resolve("../Overview")).toEqual([{ path: "docs/Overview.markdown", anchor: undefined }]);
  expect(resolve("runbooks/Details")).toEqual([{ path: "docs/guides/runbooks/Details.md", anchor: undefined }]);

  const linked = parseReadingAddress(readingLocalAddress(path, "/manual/Runbook.md#Deploy%20checks"));
  expect(linked?.sourcePath).toBe(path);
  expect(linked && resolveWiki(linked, path, files)).toEqual([{ path: "manual/Runbook.md", anchor: "Deploy checks" }]);
  for (const target of ["/manual/Missing", "/runbooks/Details", "../../../outside", "/../outside", "//server/manual/Runbook"]) {
    expect(resolve(target)).toEqual([]);
  }
});

it("honors unique names and aliases and reports duplicates and missing note embeds", () => {
  const path = "docs/start.md";
  const files = [
    { path: "Guide.md", content: "---\naliases: [\"Runbook, revisão\", 'O''Brien', Shared]\n---\nRoot version" },
    { path: "docs/Guide.md", content: "---\nalias: Shared\n---\nDocs version" },
    { path: "docs/Release.markdown", content: "---\ntitle: Published rollout\naliases:\n  - Verification guide # inherited name\n---\nRelease content" },
  ];
  const candidates = (target: string) => resolveWiki(parseWiki(target), path, files).map(({ path }) => path);
  expect(candidates("Guide")).toEqual(["Guide.md", "docs/Guide.md"]);
  expect(candidates("Shared")).toEqual(["Guide.md", "docs/Guide.md"]);
  expect(candidates("/Guide.md")).toEqual(["Guide.md"]);
  expect(candidates("release")).toEqual(["docs/Release.markdown"]);
  expect(candidates("Published rollout")).toEqual(["docs/Release.markdown"]);
  expect(candidates("Verification guide")).toEqual(["docs/Release.markdown"]);
  expect(candidates("Runbook, REVISÃO")).toEqual(["Guide.md"]);
  expect(candidates("O'Brien")).toEqual(["Guide.md"]);
  expect(candidates("Unwritten")).toEqual([]);

  const content = "![[Guide|Duplicate note]]\n\n![[Unwritten|Missing note]]";
  render(<NoteReading path={path} content={content} files={files} onLink={vi.fn()} />);
  expect(screen.getByText(/Ambiguous note reference/)).toBeTruthy();
  expect(screen.getByText(/Unresolved note reference/)).toBeTruthy();
  expect(screen.queryByText("Root version")).toBeNull();
  expect(screen.queryByText("Docs version")).toBeNull();
});

it("renders full notes, a heading with descendants, and an addressed block in Reading view", () => {
  const path = "docs/start.md";
  const content = "![[/guides/Basics]]\n\n![[../Runbook#DEPLOY CHECKS]]\n\n![[./evidence#^proof]]";
  const files = [
    { path: "guides/Basics.md", content: "---\ntitle: Metadata only\n---\n# Complete guide\nFull-note body.\n\n%% Secret comment. %%" },
    { path: "Runbook.md", content: "# Runbook\n\nIntro excluded.\n\n## Deploy checks\nApproved check.\n\n### Child heading\nNested check.\n\n## Sibling heading\nSibling excluded." },
    { path: "docs/evidence.md", content: "Previous paragraph.\n\nProof started here.\nProof continued here. ^proof\n\nFollowing paragraph." },
  ];
  render(<NoteReading path={path} content={content} files={files} onLink={vi.fn()} />);
  expect(screen.getByRole("heading", { name: "Complete guide" })).toBeTruthy();
  expect(screen.getByText("Full-note body.")).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Deploy checks" })).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Child heading" })).toBeTruthy();
  expect(screen.getByText("Nested check.")).toBeTruthy();
  expect(screen.getByText(/Proof started here\.\s+Proof continued here\./)).toBeTruthy();
  for (const omitted of ["Metadata only", "Secret comment.", "Intro excluded.", "Sibling excluded.", "Previous paragraph.", "Following paragraph."]) {
    expect(screen.queryByText(omitted)).toBeNull();
  }
});

it("resolves nested links relative to the actual transcluded source", () => {
  const path = "docs/start.md";
  const content = "![[/ops/checks]]";
  const files = [
    { path: "ops/checks.md", content: "![[./deep/process]]" },
    { path: "ops/deep/process.md", content: "[[../handoff#Verification|Next step]]" },
    { path: "ops/handoff.md", content: "# Verification" },
    { path: "handoff.md", content: "# Wrong branch" },
  ];
  const rendered = prepareReading(content, path, files);
  expect(rendered).toContain("Embedded · checks.md");
  expect(rendered).toContain("Embedded · process.md");
  const encoded = rendered.match(/omni-wiki:[^)]+/)?.[0];
  expect(encoded).toBeDefined();
  const link = parseReadingAddress(encoded ?? "");
  expect(link).toMatchObject({ sourcePath: "ops/deep/process.md", alias: "Next step", target: "../handoff", anchor: "Verification" });
  expect(link && resolveWiki(link, link.sourcePath ?? path, files)).toEqual([{ path: "ops/handoff.md", anchor: "Verification" }]);
});

it("bounds unique-note and cyclic transclusion chains", () => {
  const files = [
    { path: "docs/first.md", content: "Starting content.\n\n![[./second]]" },
    { path: "docs/second.md", content: "Second content.\n\n![[./third]]" },
    { path: "docs/third.md", content: "Third content.\n\n![[./fourth]]" },
    { path: "docs/fourth.md", content: "Fourth content.\n\n![[./fifth]]" },
    { path: "docs/fifth.md", content: "Fifth content." },
  ];
  const source = files[0]!;
  const depth = prepareReading(source.content, source.path, files);
  expect(depth).toContain("Fourth content.");
  expect(depth).toContain("maximum embed depth");
  expect(depth).not.toContain("Fifth content.");

  const backEdge = [{ ...source }, { path: "docs/second.md", content: "![[/docs/first]]" }];
  const cycle = prepareReading(source.content, source.path, backEdge);
  expect(cycle).toContain("already in the current embed chain");
  expect(cycle.match(/Starting content\./g)).toHaveLength(1);
});

it("expands same-note sections and blocks and stops a repeated anchored scope", () => {
  const path = "Start.md";
  const content = ["![[#Checks]]", "", "![[#^steps]]", "", "![[#Loop]]", "", "Checks", "======", "",
    "Follow this check.", "", "## Evidence", "", "Check step one.", "Check step two. ^steps", "", "# Loop", "", "![[#Loop]]"].join("\n");
  const output = prepareReading(content, path, [{ path, content }]);
  expect(output).toContain("> Checks\n> ======");
  expect(output).toContain("> Check step one.\n> Check step two.");
  expect(output).toContain("already in the current embed chain");
  expect(output).not.toContain("maximum embed depth");
});

it("does not expose block IDs inside comments or fenced code as transclusions", () => {
  const content = "![[Evidence#^hidden]]\n\n![[Evidence#^code]]\n\n![[Evidence#^separate]]";
  const files = [{ path: "Evidence.md", content: [
    "%%", "Private comment. ^hidden", "%%", "", "```txt", "Literal code. ^code", "```", "",
    "Visible addressed paragraph.", "", "^separate", "", "Unrelated next paragraph.",
  ].join("\n") }];
  const rendered = prepareReading(content, "Start.md", files);
  expect(rendered).toContain("Visible addressed paragraph.");
  for (const omitted of ["Private comment.", "Literal code.", "Unrelated next paragraph."]) expect(rendered).not.toContain(omitted);
});

it("keeps a rejected checkout-media embed unavailable and does not open a missing catalog note", async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  vi.mocked(invoke).mockRejectedValue(new Error("select a file inside the current checkout"));
  render(<QueryClientProvider client={client}>
    <NoteReading path="docs/start.md" content={"![[/escape/private.md|Private note]]\n\n![[/escape/private.png|Private image]]"}
      files={[]} checkoutId="checkout" onLink={vi.fn()} />
  </QueryClientProvider>);
  expect(await screen.findByText("Attachment unavailable · Private image")).toBeTruthy();
  expect(screen.getByText(/Unresolved note reference/)).toBeTruthy();
  expect(screen.queryByRole("img", { name: "Private image" })).toBeNull();
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(invoke).toHaveBeenCalledWith("read_repository_attachment", { checkoutId: "checkout", path: "escape/private.png" });
});
