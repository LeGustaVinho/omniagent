import { CircleAlert } from "lucide-react";
import { ContextActions } from "@/components/ui/context-actions";
import type { CodeProblem } from "@/ide-code-editor";
import { fileName } from "@/ide-knowledge-data";

export type FileProblem = CodeProblem & { path: string };

export const problemReference = ({ path, line, column }: FileProblem) => `${path}:${line}:${column}`;
const diagnostic = (issue: FileProblem) => `${problemReference(issue)} ${issue.label}: ${issue.message}`;

export function IdeProblems({ issues, open, reveal, quickFix, copy, onContext, simulated }: {
  issues: readonly FileProblem[];
  open: (issue: FileProblem, split?: boolean) => void;
  reveal: (path: string) => void;
  quickFix: (issue: FileProblem) => void;
  copy: (text: string, status: string) => void;
  onContext?: (issue: FileProblem, instruction?: string) => void;
  simulated: boolean;
}) {
  const problemsByFile = new Map<string, FileProblem[]>();
  for (const issue of issues) {
    const previous = problemsByFile.get(issue.path);
    if (previous) previous.push(issue);
    else problemsByFile.set(issue.path, [issue]);
  }
  return <ul className="ide-problems">
    {issues.length ? issues.map((issue, index) => {
      const { path, label, message, line, column } = issue;
      const reference = problemReference(issue);
      const fileIssues = problemsByFile.get(path)!;
      return <li key={`${reference}:${index}`}>
        <ContextActions label={reference} groups={[[
          { label: "Go to problem", run: () => open(issue) },
          { label: "Open to the side", run: () => open(issue, true) },
          ...(issue.quickFix ? [{ label: "Quick Fix…", run: () => quickFix(issue) }] : []),
          { label: "Reveal in Explorer tree", run: () => reveal(path) },
        ], [
          { label: "Copy diagnostic", run: () => copy(diagnostic(issue), "Copied diagnostic") },
          { label: "Copy diagnostic message", run: () => copy(message, "Copied diagnostic message") },
          { label: "Copy location", run: () => copy(reference, "Copied problem location") },
          ...(fileIssues.length > 1 ? [{ label: "Copy diagnostics in this file", run: () => copy(fileIssues.map(diagnostic).join("\n"), "Copied file diagnostics") }] : []),
        ], onContext ? [
          { label: "Add diagnostic to conversation…", run: () => onContext(issue) },
          { label: "Explain diagnostic…", run: () => onContext(issue, "Explain this diagnostic and suggest how to resolve it.") },
          { label: "Fix diagnostic…", run: () => onContext(issue, "Investigate and fix the problem described by this diagnostic.") },
        ] : []]}>
          <button className="ide-problem-open" type="button" onClick={() => open(issue)} title={reference}>
            <CircleAlert size={15} /><span>{label} · {message}</span><small>{fileName(path)}:{line}:{column}</small>
          </button>
        </ContextActions>
      </li>;
    }) : <li className="file-help">No editor diagnostics in this workspace.</li>}
    {simulated && <li className="file-help">ripgrep missing · Configure its executable in Settings to enable live repository search.</li>}
  </ul>;
}
