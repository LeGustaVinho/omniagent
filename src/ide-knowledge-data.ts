export interface ExampleFile {
  path: string;
  content: string;
}

export const deliveryNote = "docs/release.md";

export const exampleNotes: ExampleFile[] = [
  { path: deliveryNote, content: [
    "---",
    "title: Release readiness",
    "owner: Platform",
    "status: in-review",
    "tags: [release, windows]",
    "---",
    "",
    "# Release readiness",
    "",
    "First release checklist for the Windows desktop workspace. The task should add a readiness section to the app and verify that it can be reached with the keyboard.",
    "",
    "> [!NOTE] Review boundary",
    "> Run **Plan Mode** before writing. The implementation stage requires approval of the exact plan.",
    "",
    "## Acceptance criteria",
    "",
    "- [x] Identify the startup checks.",
    "- [ ] Show actionable failures for missing tools.",
    "- [ ] Verify that existing worktrees can still be restored.",
    "",
    "## References",
    "",
    "Start with [[architecture|architecture overview]] and [[runner#Startup checks|runner diagnostics]]. See [[Readme]] for local conventions.",
    "",
    "![[runbook#Recovery]]",
    "",
    "### Evidence",
    "",
    "| Check | Evidence |",
    "| --- | --- |",
    "| Startup | Workspace smoke test |",
    "| Recovery | Reopen the saved attempt |",
    "",
    "Coverage can be tracked as $c = p / n$. ==Sign off after both checks==. #release",
    "",
    "Open the [[release-evidence.pdf|evidence PDF]] or [[rollout-tone.wav|release cue]].",
    "",
    "%% Keep a note on tool availability for later review. %%",
    "",
    "```mermaid",
    "flowchart LR",
    "  Note --> Plan",
    "  Plan --> Approval",
    "  Approval --> Review",
    "```",
  ].join("\n") },
  { path: "docs/architecture.md", content: [
    "---",
    "title: Workspace architecture",
    "tags: [design]",
    "---",
    "",
    "# Workspace architecture",
    "",
    "Projects own canonical repository and checkout identities. The Rust host owns admission, Git, durable state and runner supervision.",
    "",
    "## Scope",
    "",
    "Use [[runner]] to check readiness before launching a task. Review [[release]] before publishing.",
    "",
    "The workspace binds each view to a checkout revision. ^checkout-scope",
  ].join("\n") },
  { path: "docs/runner.md", content: [
    "---",
    "title: Runner diagnostics",
    "tags: [windows, design]",
    "---",
    "",
    "# Runner diagnostics",
    "",
    "## Startup checks",
    "",
    "- Verify executable discovery and protocol handshake.",
    "- Inspect runner information in Settings.",
    "- Show an actionable error if a prerequisite is unavailable.",
    "",
    "See [[architecture#^checkout-scope|checkout scope]] and [[missing-runner-profile]].",
  ].join("\n") },
  { path: "docs/ops/runbook.md", content: [
    "# Operations runbook",
    "",
    "## Recovery",
    "",
    "Open the interrupted attempt, review its evidence and explicitly resume the queued work.",
    "",
    "## Evidence",
    "",
    "The proposed change originated in [[release]].",
  ].join("\n") },
  { path: "docs/ops/Readme.md", content: "# Operations conventions\n\nFollow [[runbook]] when an attempt is interrupted.\n" },
  { path: "docs/team/Readme.md", content: "# Team conventions\n\nReview [[architecture]] before moving shared interfaces.\n" },
];

export const exampleSources: ExampleFile[] = [
  { path: "src/health.ts", content: [
    "export type ToolStatus = \"ready\" | \"missing\" | \"blocked\";",
    "",
    "export interface StartupCheck {",
    "  name: string;",
    "  status: ToolStatus;",
    "  detail: string;",
    "}",
    "",
    "export const startupChecks: StartupCheck[] = [",
    "  { name: \"Git\", status: \"ready\", detail: \"Repository available\" },",
    "  { name: \"ripgrep\", status: \"missing\", detail: \"Configure executable path\" },",
    "];",
    "",
    "export function readyForWork(checks: StartupCheck[]): boolean {",
    "  return checks.every((check) => check.status === \"ready\");",
    "}",
    "",
  ].join("\n") },
  { path: "src/index.ts", content: [
    "import { readyForWork, startupChecks } from \"./health\";",
    "",
    "export function summarizeStartup(): string {",
    "  return readyForWork(startupChecks) ? \"Ready\" : \"Review prerequisites\";",
    "}",
    "",
  ].join("\n") },
  { path: "tests/health.test.ts", content: [
    "import { describe, expect, it } from \"vitest\";",
    "import { readyForWork } from \"../src/health\";",
    "",
    "describe(\"startup checks\", () => {",
    "  it(\"blocks launch when a required tool is missing\", () => {",
    "    expect(readyForWork([{ name: \"Git\", status: \"missing\", detail: \"Missing\" }])).toBe(false);",
    "  });",
    "});",
    "",
  ].join("\n") },
  { path: "package.json", content: '{\n  "name": "example-workspace",\n  "private": true,\n  "scripts": { "test": "vitest run" }\n}\n' },
];

export function fileName(path: string) {
  return path.split("/").at(-1) ?? path;
}

export function normalizeRepositoryPath(path: string): string | undefined {
  if (/^(?:[\\/]|[a-z]:)/i.test(path) || path.includes("\0")) return;
  const output: string[] = [];
  for (const part of path.replaceAll("\\", "/").split("/")) {
    if (part === "..") {
      if (!output.length) return;
      output.pop();
    } else if (part && part !== ".") {
      output.push(part);
    }
  }
  return output.length ? output.join("/") : undefined;
}

export interface FileSearchResult {
  path: string;
  line: number;
  excerpt: string;
  columns?: readonly number[];
  occurrences?: number;
  lineText?: string | null;
}

export function findFileText(files: readonly ExampleFile[], query: string, content: boolean): FileSearchResult[] {
  const term = query.trim().toLowerCase();
  if (!term) return [];
  const output: FileSearchResult[] = [];
  for (const file of files) {
    if (file.path.toLowerCase().includes(term)) output.push({ path: file.path, line: 1, excerpt: file.path });
    if (!content) continue;
    for (const [index, line] of file.content.split("\n").entries()) {
      const position = line.toLowerCase().indexOf(term);
      if (position >= 0) output.push({ path: file.path, line: index + 1, excerpt: line.trim(), columns: [position + 1], lineText: line });
      if (output.length >= 100) return output;
    }
  }
  return output;
}
