import { expect, it } from "vitest";
import { omniAgentFileLink } from "@/file-links";
import { openWorkspaceFile, readSourceLineTarget } from "@/file-context-actions";
import { readNoteSearchTarget } from "@/ide-file-search";
import type { WorkspaceId } from "@/workbench-contract";

it("copies an encoded link with its checkout and supported cursor coordinates", () => {
  const id = "619536f7-bac1-46ea-af56-88cc8b22013a";
  expect(omniAgentFileLink(id, "src/Revisão #a.ts", 7, 3)).toBe(
    `omniagent://open-file/?checkout=${id}&path=src%2FRevis%C3%A3o+%23a.ts&line=7&column=3`,
  );
  expect(omniAgentFileLink(id, "docs/Note.MD", 12, 3)).toBe(
    `omniagent://open-file/?checkout=${id}&path=docs%2FNote.MD&line=12`,
  );
});

it("routes source coordinates, Markdown lines, and repeated file links", () => {
  const opens: Array<[WorkspaceId, string | undefined]> = [];
  const open = (workspace: WorkspaceId, target?: string) => opens.push([workspace, target]);
  openWorkspaceFile(open, "src/index.ts", 3, { column: 2 });
  openWorkspaceFile(open, "docs/Note.MD", 12);
  openWorkspaceFile(open, "src/index.ts", undefined, { repeat: true });
  openWorkspaceFile(open, "src/index.ts", undefined, { repeat: true });
  expect(opens.map(([workspace]) => workspace)).toEqual(["ide", "knowledge", "ide", "ide"]);
  expect(readSourceLineTarget(opens[0]![1])).toEqual({ path: "src/index.ts", line: 3, column: 2 });
  expect(readNoteSearchTarget(opens[1]![1]!)).toEqual({ path: "docs/Note.MD", line: 12 });
  expect(readSourceLineTarget(opens[2]![1])).toEqual({ path: "src/index.ts" });
  expect(opens[2]![1]).not.toBe(opens[3]![1]);
});
