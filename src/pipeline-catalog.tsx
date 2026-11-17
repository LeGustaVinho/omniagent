import { queryOptions } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Plus, Search } from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { type StageKind } from "@/pipelines-model";
import { portType, type WorkflowDefinition, type WorkflowPort } from "@/workflow-graph-model";

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
  return <PipelineNodeCatalog groups={Object.entries(registry).map(([title, nodes]) => ({
    title,
    entries: nodes.map(({ label, input, output, kind }) => ({
      id: label, label, input, output, action: onAdd && kind ? () => onAdd(kind, label) : undefined,
    })),
  }))} />;
}

export type PipelineCatalogEntry = {
  id: string;
  label: string;
  input: string;
  output: string;
  detail?: string;
  action?: () => void;
  actionLabel?: string;
  unavailable?: string;
};

export type PipelineCatalogGroup = { title: string; entries: PipelineCatalogEntry[] };

const catalogGroups = [
  ["Entry and results", ["start", "typed_start", "load_context", "load_work_context", "summarize_result", "result"]],
  ["Data", ["build_list", "set_fields", "filter_map_list", "build_text", "validate_output", "collect_evidence"]],
  ["Control", ["condition", "parallel", "join", "for_each", "repeat_until", "iteration_limit", "subflow"]],
  ["Decisions", ["request_input", "approval", "approve_implementation"]],
  ["Agile", ["link_work", "find_work", "update_progress", "manage_work", "materialize_backlog", "run_backlog"]],
  ["Agents and execution", ["agent", "plan", "review_plan", "implement", "review_candidate", "compare_models", "command", "mcp_tool"]],
] as const;

function groupFor(kind: string) {
  return catalogGroups.find(([, kinds]) => (kinds as readonly string[]).includes(kind))?.[0] ?? "Git and quality";
}

function describePorts(ports: WorkflowPort[]) {
  return ports.map(({ name, value }) => `${name} (${portType(value)})`).join(", ") || "None";
}

export const workflowGraphRegistryQuery = queryOptions({
  queryKey: ["workflow-graph-registry"], staleTime: Infinity,
  queryFn: () => invoke<WorkflowDefinition[]>("workflow_graph_registry"),
});

export function workflowRegistryGroups(definitions: WorkflowDefinition[],
  configure: (definition: WorkflowDefinition) => Pick<PipelineCatalogEntry, "action" | "unavailable"> = () => ({})): PipelineCatalogGroup[] {
  const groups = [...catalogGroups.map(([title]) => title), "Git and quality"];
  return groups.map((title) => ({
    title,
    entries: definitions.filter(({ kind }) =>
      !["for_each_begin", "for_each_gate", "for_each_collect", "repeat_output"].includes(kind) && groupFor(kind) === title)
      .map((entry) => ({
        id: entry.kind,
        label: entry.kind === "result" ? "Delivery result" : entry.title,
        input: describePorts(entry.inputs),
        output: describePorts(entry.outputs),
        ...configure(entry),
      })),
  }));
}

export function PipelineNodeCatalog({ groups, searchLabel = "Search pipeline nodes", compact = false }: {
  groups: PipelineCatalogGroup[]; searchLabel?: string; compact?: boolean;
}) {
  const [query, setQuery] = useState("");
  const terms = query.trim().toLowerCase().split(/\s+/);
  const matches = groups.map(({ title, entries }) => ({
    title,
    entries: entries.filter(({ label, input, output, detail }) => terms.every((term) =>
      [title, label, input, output, detail].join(" ").toLowerCase().includes(term))),
  })).filter(({ entries }) => entries.length > 0);
  return (
    <div className="pipeline-catalog" data-compact={compact || undefined}>
      <div className="pipeline-catalog-toolbar">
        <label className="pipeline-catalog-search"><Search size={16} aria-hidden="true" />
          <input type="search" aria-label={searchLabel} placeholder="Find a node, port or preset" value={query}
            onChange={(event) => setQuery(event.target.value)} />
        </label>
      </div>
      {!matches.length && <p className="pipeline-note" role="status">No matching nodes.</p>}
      {matches.map(({ title, entries }) => <section key={title} className="pipeline-catalog-group">
        <h3>{title}</h3>
        <ul>{entries.map((node) => <li key={node.id}>
          <div className="pipeline-catalog-node">
            <div><h4>{node.label}</h4>
              <dl><dt>Input</dt><dd>{node.input}</dd><dt>Output</dt><dd>{node.output}</dd></dl>
              {node.detail && <p className="pipeline-catalog-detail">{node.detail}</p>}
            </div>
            {node.action
              ? <Button variant="outline" type="button" aria-label={node.actionLabel ?? `Add ${node.label}`} onClick={node.action}><Plus size={15} /> Add</Button>
              : <span className="pipeline-meta">{node.unavailable ?? "Definition preview"}</span>}
          </div>
        </li>)}</ul>
      </section>)}
    </div>
  );
}
