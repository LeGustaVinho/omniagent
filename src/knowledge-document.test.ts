import { describe, expect, it } from "vitest";
import {
  anchorLine, getProperties, headings, parseReadingAddress, parseWiki, prepareReading, repairWikiAddress,
  resolveWiki, rewriteNoteReferences, updateProperty, wikiLinks,
} from "@/knowledge-document";
import { exampleNotes } from "@/ide-knowledge-data";

describe("Knowledge example document projection", () => {
  it("follows qualified paths, preserves an anchor and surfaces duplicate names", () => {
    expect(resolveWiki(parseWiki("Readme"), "docs/release.md", exampleNotes)).toHaveLength(2);
    expect(resolveWiki(parseWiki("ops/Readme"), "docs/release.md", exampleNotes)).toEqual([{ path: "docs/ops/Readme.md", anchor: undefined }]);
    expect(resolveWiki(parseWiki("architecture#^checkout-scope|checkout"), "docs/runner.md", exampleNotes))
      .toEqual([{ path: "docs/architecture.md", anchor: "^checkout-scope" }]);
    expect(resolveWiki(parseWiki("../../outside"), "docs/release.md", exampleNotes)).toEqual([]);
    expect(anchorLine("# One\n\nLine. ^abc\n", "^abc")).toBe(3);
  });

  it("rewrites note links outside protected markup and frontmatter", () => {
    const code = "`[[architecture]]`";
    const fence = "``` markdown\r\n[[architecture]]\r\n```";
    const source = "\uFEFF---\r\nowner: Alpha # keep this\r\nexample: '[[architecture]]'\r\n---\r\n"
      + "%% [[architecture]] %%\r\n" + code + "\r\n" + fence + "\r\n"
      + "![[" + "architecture#Scope|overview" + "]]\r\n";
    expect(wikiLinks(source).map(({ raw }) => raw)).toEqual(["architecture#Scope|overview"]);
    expect(rewriteNoteReferences(source, "docs/release.md", "docs/architecture.md", "docs/archive/architecture.md", exampleNotes))
      .toBe(source.replace("architecture#Scope|overview", "docs/archive/architecture.md#Scope|overview"));
    expect(updateProperty(source, "owner", "Beta")).toBe(source.replace("owner: Alpha", "owner: Beta"));
    expect(getProperties(source)[0]?.value).toBe("Alpha");
  });

  it("repairs an unavailable heading and bounds circular transclusions", () => {
    const source = "Read [[old#Missing|the guide]].";
    expect(repairWikiAddress(source, "old#Missing|the guide", "docs/runner.md", "Startup checks"))
      .toBe("Read [[docs/runner.md#Startup checks|the guide]].");

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
