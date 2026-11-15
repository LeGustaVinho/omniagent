import { exampleNotes, exampleSources } from "@/ide-knowledge-data";

export type ChangeKind = "Added" | "Modified" | "Deleted" | "Renamed" | "Binary" | "Untracked";
export type ChangesSource = "working" | "staged" | "branch" | "saved";

export interface ChangedFile {
  path: string;
  previousPath?: string;
  kind: ChangeKind;
  language: string;
  additions: number;
  deletions: number;
  original: string;
  modified: string;
  patch?: string;
}

function appendChange(path: string, language: string, newLines: string[]): ChangedFile {
  const original = [...exampleNotes, ...exampleSources].find((file) => file.path === path)?.content ?? "";
  const originalLines = original.replace(/\n$/, "").split("\n");
  const context = originalLines.slice(-3);
  const start = originalLines.length - context.length + 1;
  return {
    path,
    kind: "Modified",
    language,
    additions: newLines.length,
    deletions: 0,
    original,
    modified: [...originalLines, ...newLines, ""].join("\n"),
    patch: [`@@ -${start},${context.length} +${start},${context.length + newLines.length} @@`,
      ...context.map((line) => ` ${line}`), ...newLines.map((line) => `+${line}`)].join("\n"),
  };
}

export const savedCandidate: readonly ChangedFile[] = [
  appendChange("docs/release.md", "markdown", [
    "", "## Recovery", "",
    "- Open the retained attempt from [[ops/runbook#Recovery|the recovery runbook]].",
    "- Verify that the previous build can reopen the same worktree.",
    "- Record the packaged Windows smoke check with the accepted diff.",
  ]),
  appendChange("src/health.ts", "typescript", [
    "", "export function missingPrerequisites(checks: StartupCheck[]): string[] {",
    '  return checks.filter((check) => check.status !== "ready").map((check) => check.name);',
    "}",
  ]),
  appendChange("tests/health.test.ts", "typescript", [
    "", 'it("recognizes a verified recovery runner", () => {',
    '  expect(readyForWork([{ name: "Recovery runner", status: "ready", detail: "Verified" }])).toBe(true);',
    "});",
  ]),
];

export const workingChanges: ChangedFile[] = [
  ...savedCandidate,
  { path: "docs/runner-notes.md", kind: "Untracked", language: "markdown", additions: 3, deletions: 0, original: "", modified: "# Runner notes\n\nCheck active writers before removing a checkout.\n" },
  { path: "assets/setup-flow.png", kind: "Binary", language: "plaintext", additions: 0, deletions: 0, original: "", modified: "" },
];

export const branchChanges: ChangedFile[] = [
  ...savedCandidate,
  { path: "docs/windows-release.md", previousPath: "docs/packaging.md", kind: "Renamed", language: "markdown", additions: 0, deletions: 0, original: "# Windows packaging\n", modified: "# Windows packaging\n" },
  { path: "docs/legacy-updater.md", kind: "Deleted", language: "markdown", additions: 0, deletions: 3, original: "# Legacy updater\n\nRestart through the old helper.\n", modified: "" },
];

export const releaseCommits = [
  { id: "d73b42a", title: "Verify the release readiness criteria", branch: "agent/release-checklist", author: "Implementation agent", date: "Aug 20, 10:23" },
  { id: "1ba6dc2", title: "Document the Windows upgrade workflow", branch: "main", author: "Release team", date: "Aug 19, 16:34" },
  { id: "72b39ac", title: "Record initial diagnostics", branch: "main", author: "Diagnostics", date: "Aug 18, 11:05" },
];
