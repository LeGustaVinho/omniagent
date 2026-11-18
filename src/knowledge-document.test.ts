import { describe, expect, it } from "vitest";
import {
  anchorLine, getProperties, headings, markdownNoteLink, noteMarkdownLink, parseReadingAddress, parseWiki, prepareReading,
  resolveWiki, wikiLinks,
} from "@/knowledge-document";
import type { ExampleFile } from "@/ide-knowledge-data";

const notes: ExampleFile[] = [
  { path: "docs/architecture.md", content: "# Scope\nCheckout scope. ^checkout-scope" },
  { path: "docs/ops/Readme.md", content: "# Operations conventions" },
  { path: "docs/team/Readme.md", content: "# Team conventions" },
];

describe("Knowledge document projection", () => {
  it("copies a valid Markdown link to the exact checkout-root note", () => {
    const path = "ops décisões/Ação [QA] (#50%).md";
    const destination = "/ops%20d%C3%A9cis%C3%B5es/A%C3%A7%C3%A3o%20%5BQA%5D%20%28%2350%25%29.md";
    expect(noteMarkdownLink(path)).toBe(`[Ação \\[QA\\] (#50%)](${destination})`);
    const link = markdownNoteLink("docs/Start.md", destination)!;
    expect(resolveWiki(link, "docs/Start.md", [{ path, content: "" }, { path: "docs/" + path, content: "" }]))
      .toEqual([{ path, anchor: undefined }]);
  });

  it("follows qualified paths, preserves an anchor and surfaces duplicate names", () => {
    expect(resolveWiki(parseWiki("Readme"), "docs/release.md", notes)).toHaveLength(2);
    expect(resolveWiki(parseWiki("ops/Readme"), "docs/release.md", notes)).toEqual([{ path: "docs/ops/Readme.md", anchor: undefined }]);
    expect(resolveWiki(parseWiki("architecture#^checkout-scope|checkout"), "docs/runner.md", notes))
      .toEqual([{ path: "docs/architecture.md", anchor: "^checkout-scope" }]);
    expect(resolveWiki(parseWiki("../../outside"), "docs/release.md", notes)).toEqual([]);
    expect(anchorLine("# One\n\nLine. ^abc\n", "^abc")).toBe(3);
  });

  it("finds only note links outside protected markup and frontmatter", () => {
    const code = "`[[architecture]]`";
    const fence = "``` markdown\r\n[[architecture]]\r\n```";
    const source = "\uFEFF---\r\nowner: Alpha # keep this\r\nexample: '[[architecture]]'\r\n---\r\n"
      + "%% [[architecture]] %%\r\n" + code + "\r\n" + fence + "\r\n"
      + "![[" + "architecture#Scope|overview" + "]]\r\n";
    expect(wikiLinks(source).map(({ raw }) => raw)).toEqual(["architecture#Scope|overview"]);
    expect(getProperties(source)[0]?.value).toBe("Alpha");
  });

  it("bounds circular transclusions", () => {
    const catalog = [{ path: "a.md", content: "# A\n\n![[b]]" }, { path: "b.md", content: "# B\n\n![[a]]" }];
    expect(prepareReading(catalog[0]!.content, "a.md", catalog)).toContain("already in the current embed chain");
  });

  it("keeps original editor line numbers when outline skips properties and comments", () => {
    const source = "---\n# Frontmatter comment\ntitle: Example\n---\n%%\n# Hidden\n%%\n# Visible\n";
    expect(headings(source)).toEqual([{ anchor: "Visible", label: "Visible", level: 1, line: 8 }]);
    expect(prepareReading(source, "example.md", [])).toContain("# Visible");
    expect(prepareReading(source, "example.md", [])).not.toContain("# Hidden");
  });

  it("keeps relative wikilinks in nested transclusions scoped to the source note", () => {
    const files = [
      { path: "docs/intro.md", content: "![[/ops/checks#Approval]]" },
      { path: "ops/checks.markdown", content: "# Approval\nReview [[./Handoff#Verify|the next step]].\n# Unrelated\nWrong section" },
      { path: "ops/Handoff.md", content: "---\naliases:\n  - \"Review, confirm\"\n  - Handoff guide # preserve\n---\n## Verify" },
      { path: "docs/Handoff.md", content: "# Wrong note" },
    ];
    const rendered = prepareReading(files[0]!.content, files[0]!.path, files);
    const link = parseReadingAddress(rendered.match(/omni-wiki:[^)]+/)![0])!;
    expect(link.sourcePath).toBe("ops/checks.markdown");
    expect(resolveWiki(link, link.sourcePath!, files)).toEqual([{ path: "ops/Handoff.md", anchor: "Verify" }]);
    expect(resolveWiki(parseWiki("Review, confirm"), files[0]!.path, files)).toEqual([{ path: "ops/Handoff.md", anchor: undefined }]);
    expect(resolveWiki(parseWiki("Handoff guide"), files[0]!.path, files)).toEqual([{ path: "ops/Handoff.md", anchor: undefined }]);
    expect(rendered).not.toContain("Wrong section");
    expect(prepareReading("%%\n![[/ops/checks]]\n%%\nAfter", files[0]!.path, files)).not.toContain("Embedded");
  });
});
