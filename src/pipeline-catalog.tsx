import { Plus, Search } from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { type StageKind } from "@/pipelines-model";

type CatalogNode = {
  label: string;
  input: string;
  output: string;
  kind?: StageKind;
};

const registry: Record<string, CatalogNode[]> = {
  "Entry and results": [
    { label: "Start", input: "Manual input or Agile event", output: "Work reference + inputs" },
    { label: "Load context", input: "Work reference + revision", output: "Markdown + checklist" },
    { label: "Result", input: "Summary + evidence", output: "Result artifact" },
  ],
  Data: [
    { label: "Set fields", input: "Object + mappings", output: "Object" },
    { label: "Filter and map list", input: "List + predicate + mapping", output: "List" },
    { label: "Build text", input: "Values + Markdown template", output: "Text" },
  ],
  Control: [
    { label: "Condition", kind: "condition", input: "Structured values", output: "Selected branch" },
    { label: "Parallel", input: "Control", output: "Independent branches" },
    { label: "Join", kind: "join", input: "Selected branches + results", output: "Combined result" },
    { label: "For each", input: "List + subflow", output: "Ordered results" },
    { label: "Repeat until", input: "Input + predicate + bound", output: "Last result" },
  ],
  "Agents and execution": [
    { label: "Agent", kind: "review", input: "Role + prompt + context", output: "Structured agent result" },
    { label: "Compare models", input: "Prompt + read-only context + models", output: "Individual attempts" },
    { label: "Command", input: "Local binding + arguments + stdin", output: "Exit code + logs + text or JSON" },
    { label: "MCP tool", input: "Server binding + schema inputs", output: "Typed result or artifact" },
  ],
  Decisions: [
    { label: "Request input", input: "Question schema", output: "Typed answer" },
    { label: "Approval", input: "Evidence + decision scope", output: "Approved / declined" },
    { label: "Approve implementation", kind: "approval", input: "Plan revision + repository scope", output: "Scoped implementation release" },
  ],
  Agile: [
    { label: "Find work", input: "Filters + project", output: "Work references" },
    { label: "Create or link work", kind: "task", input: "Work fields + source identity", output: "Stable work reference" },
    { label: "Update progress", input: "Work reference + change + revision", output: "Recorded progress change" },
  ],
  "Git and quality": [
    { label: "Prepare worktree", input: "Work reference + base revision", output: "Worktree + branch" },
    { label: "Read Git", input: "Checkout or saved evidence", output: "Status + branch + SHA + diff" },
    { label: "Verify candidate", kind: "verify", input: "Captured candidate + checks", output: "Verification evidence" },
    { label: "Accept diff", kind: "accept", input: "Candidate + verification", output: "Acceptance record" },
    { label: "Commit", kind: "commit", input: "Accepted candidate + expected ref", output: "Verified source commit" },
    { label: "Merge", kind: "merge", input: "Verified source commit + expected destination", output: "Verified integration" },
  ],
  Presets: [
    { label: "Plan", kind: "plan", input: "Role + scoped context", output: "Plan revision" },
    { label: "Implement", kind: "implementation", input: "Plan + release + worktree", output: "Captured candidate" },
    { label: "Review", kind: "review", input: "Scope + candidate", output: "Review findings" },
    { label: "Build", input: "Registered build command", output: "Build evidence" },
    { label: "Lint", input: "Registered lint command", output: "Lint evidence" },
    { label: "Test", input: "Registered test command", output: "Test evidence" },
    { label: "Review and test", input: "Candidate + checks", output: "Review and verification evidence" },
  ],
};

export function PipelineCatalog({ onAdd }: {
  onAdd?: (kind: StageKind, title: string) => void;
}) {
  const [query, setQuery] = useState("");
  const terms = query.trim().toLowerCase().split(/\s+/);
  const groups = Object.entries(registry).map(([group, entries]) => ({
    group,
    nodes: entries.filter((entry) => terms.every((term) =>
      [group, entry.label, entry.input, entry.output].join(" ").toLowerCase().includes(term))),
  })).filter(({ nodes }) => nodes.length);
  return (
    <div className="pipeline-catalog">
      <div className="pipeline-catalog-toolbar">
        <label className="pipeline-catalog-search"><Search size={16} aria-hidden="true" />
          <input type="search" aria-label="Search pipeline nodes" placeholder="Find a node, port or preset" value={query}
            onChange={(event) => setQuery(event.target.value)} />
        </label>
      </div>
      {!groups.length && <p className="pipeline-note" role="status">No matching nodes.</p>}
      {groups.map(({ group, nodes }) => <section key={group} className="pipeline-catalog-group">
        <h3>{group}</h3>
        <ul>{nodes.map((node) => <li key={node.label}>
          <div className="pipeline-catalog-node">
            <div><h4>{node.label}</h4>
              <dl><dt>Input</dt><dd>{node.input}</dd><dt>Output</dt><dd>{node.output}</dd></dl>
            </div>
            {onAdd && node.kind
              ? <Button variant="outline" type="button" aria-label={`Add ${node.label}`} onClick={() => onAdd(node.kind!, node.label)}><Plus size={15} /> Add</Button>
              : <span className="pipeline-meta">Definition preview</span>}
          </div>
        </li>)}</ul>
      </section>)}
    </div>
  );
}
