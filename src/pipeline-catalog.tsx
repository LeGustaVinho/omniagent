import { queryOptions } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import {
  ArrowUpRight, Blocks, CirclePlay, Filter, GitBranch, GitPullRequest, ListChecks, Network,
  Plus, Search, SearchX, Settings2, ShieldCheck, Waypoints, X, type LucideIcon,
} from "lucide-react";
import { useId, useRef, useState, type ReactNode } from "react";
import { Dialog } from "radix-ui";
import { Button } from "@/components/ui/button";
import { PipelineNodeDocumentation } from "@/pipeline-node-details";
import { type StageKind } from "@/pipelines-model";
import { portType, type WorkflowDefinition, type WorkflowPort } from "@/workflow-graph-model";
import "./pipeline-library.css";

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

export function PipelineCatalog({ onAdd, library = false }: {
  onAdd?: (kind: StageKind, title: string) => void;
  library?: boolean;
}) {
  const groups = Object.entries(registry).map(([title, nodes]) => ({
    title,
    entries: nodes.map(({ label, input, output, kind }) => ({
      id: label, label, input, output, purpose: examplePurposes[label],
      simulation: { label, stage: kind, input, output },
      action: onAdd && kind ? () => onAdd(kind, label) : undefined,
    })),
  }));
  return library ? <PipelineLibraryCatalog groups={groups} /> : <PipelineNodeCatalog groups={groups} />;
}

export type PipelineCatalogEntry = {
  id: string;
  label: string;
  input: string;
  output: string;
  detail?: string;
  purpose?: string;
  definition?: WorkflowDefinition;
  simulation?: { label: string; stage?: StageKind; input: string; output: string };
  details?: ReactNode;
  action?: () => void;
  actionLabel?: string;
  unavailable?: string;
};

export type PipelineCatalogGroup = { title: string; entries: PipelineCatalogEntry[] };

const categoryIcons: Record<string, LucideIcon> = {
  "Entry and results": CirclePlay,
  Data: Blocks,
  Control: Waypoints,
  Decisions: ShieldCheck,
  Agile: ListChecks,
  "Agents and execution": Network,
  "Git and quality": GitBranch,
  "GitHub": GitPullRequest,
  Presets: Settings2,
  "Published presets": Settings2,
  "Published subflows": Waypoints,
};

export const nodePurposes: Partial<Record<string, string>> = {
  validate_output: "Validate an upstream result against a JSON schema before the next step consumes it.",
  collect_evidence: "Gather evidence from incoming branches and keep failed or skipped outcomes visible.",
  quality_gate: "Evaluate rules against collected evidence and decide whether to continue, revise, or stop for review.",
  materialize_backlog: "Create an Epic and its dependent tasks from a validated plan, SDD, and BDD.",
  run_backlog: "Launch the pinned child workflow for eligible tasks when their dependencies have integrated.",
  verify_test_phase: "Record a baseline, expected red failure, or green pass against pinned test and repository revisions.",
  verify_integrated_checks: "Run registered checks on the exact Git revision produced by backlog integration.",
  search_public_repositories: "Collect public repository research for the query provided by the workflow.",
  open_or_update_pull_request: "Open or update a pull request for the branch recorded in verified Git evidence.",
  wait_for_ci: "Check required CI for the recorded pull request head and base.",
  merge_pull_request: "Integrate a checked pull request and record verified merge evidence.",
  agent: "Ask a read-only agent to complete a prompt.",
  typed_start: "Start with configurable fields, a note, or an Agile event.",
  summarize_result: "Summarize workflow evidence.",
  load_work_context: "Read task details, a saved Markdown document, or both.",
  manage_work: "Create or link a task using configured work fields.",
  set_fields: "Build or update fields in a data object.",
  filter_map_list: "Keep matching items and transform their fields.",
  build_text: "Fill a text template with values.",
  request_input: "Pause for an answer and return the submitted fields.",
  approval: "Pause for a decision and branch on the response.",
  compare_models: "Run the same request against selected models.",
  find_work: "Look up matching tasks or work items.",
  update_progress: "Record progress or update a checklist.",
  start: "Start from a saved Markdown document.",
  load_context: "Pass a saved Markdown revision to the next step.",
  plan: "Ask a planning agent to draft a plan.",
  review_plan: "Ask a read-only agent to review a plan.",
  link_work: "Create or link a task from a reviewed plan.",
  approve_implementation: "Record approval before implementation.",
  prepare_worktree: "Prepare a managed worktree for approved work.",
  implement: "Apply an approved plan in its worktree.",
  review_candidate: "Review the candidate's changes.",
  read_git: "Inspect the selected Git state and diff.",
  review_diff_candidate: "Ask an agent to review the selected diff.",
  verify_review_diff: "Run the required checks for the selected diff.",
  publish_diff_report: "Combine the selected diff, review and checks into a report.",
  verify_candidate: "Run the required checks against a candidate.",
  accept_candidate: "Record acceptance of a reviewed and checked candidate.",
  verify_commit: "Check the accepted candidate's commit.",
  verify_merge: "Check that the commit is integrated into the destination.",
  activate_maintenance: "Record the approved maintenance candidate for activation.",
  command: "Run an approved command in its worktree.",
  mcp_tool: "Run a registered MCP tool.",
  condition: "Take a branch based on a predicate.",
  parallel: "Start multiple branches at once.",
  join: "Wait for branches before continuing.",
  subflow: "Call another workflow in this one.",
  repeat_until: "Repeat a subflow until a condition or limit is reached.",
  iteration_limit: "End the workflow when a repetition limit is reached.",
  build_list: "Build a list of items for later steps.",
  for_each: "Run a subflow for each item in a list.",
  result: "Record the delivered work and Git evidence.",
};

const examplePurposes: Record<string, string | undefined> = {
  Start: "Start from a work reference and inputs.",
  "Load context": "Load the Markdown and checklist for a saved revision.",
  Result: "Package the workflow summary and evidence.",
  "Set fields": nodePurposes.set_fields,
  "Filter and map list": nodePurposes.filter_map_list,
  "Build text": nodePurposes.build_text,
  Condition: nodePurposes.condition,
  Parallel: nodePurposes.parallel,
  Join: nodePurposes.join,
  "For each": nodePurposes.for_each,
  "Repeat until": nodePurposes.repeat_until,
  Agent: nodePurposes.agent,
  "Compare models": nodePurposes.compare_models,
  Command: nodePurposes.command,
  "MCP tool": nodePurposes.mcp_tool,
  "Request input": nodePurposes.request_input,
  Approval: nodePurposes.approval,
  "Approve implementation": nodePurposes.approve_implementation,
  "Find work": nodePurposes.find_work,
  "Create or link work": nodePurposes.manage_work,
  "Update progress": nodePurposes.update_progress,
  "Prepare worktree": nodePurposes.prepare_worktree,
  "Read Git": nodePurposes.read_git,
  "Verify candidate": nodePurposes.verify_candidate,
  "Accept diff": nodePurposes.accept_candidate,
  Commit: "Record the accepted candidate as a Git commit.",
  Merge: "Integrate a verified commit into its destination.",
  Plan: nodePurposes.plan,
  Implement: nodePurposes.implement,
  Review: nodePurposes.review_candidate,
  Build: "Run the registered build command.",
  Lint: "Run the registered lint command.",
  Test: "Run the registered test command.",
  "Review and test": "Review a candidate and verify the required checks.",
};

const effects: Record<string, { label: string; filter: string }> = {
  none: { label: "No side effect", filter: "Flow and decisions" },
  read_evidence: { label: "Reads evidence", filter: "Read and verify" },
  invoke_model: { label: "Calls a model", filter: "Model calls" },
  change_task_records: { label: "Changes task records", filter: "Task changes" },
  release_implementation: { label: "Records implementation approval", filter: "Flow and decisions" },
  provision_managed_checkout: { label: "Prepares a managed worktree", filter: "Worktree preparation" },
  write_repository: { label: "Writes to the repository", filter: "Repository writes" },
  invoke_local_tool: { label: "Calls a local tool", filter: "Tool calls" },
  verify_candidate: { label: "Verifies the candidate", filter: "Read and verify" },
  record_acceptance: { label: "Records acceptance", filter: "Flow and decisions" },
  verify_repository_commit: { label: "Verifies a Git commit", filter: "Read and verify" },
  verify_repository_merge: { label: "Verifies a Git merge", filter: "Read and verify" },
  record_maintenance_activation: { label: "Records maintenance activation", filter: "Flow and decisions" },
};

const catalogGroups = [
  ["Entry and results", ["start", "typed_start", "load_context", "load_work_context", "summarize_result", "result"]],
  ["Data", ["build_list", "set_fields", "filter_map_list", "build_text", "validate_output", "collect_evidence"]],
  ["Control", ["condition", "parallel", "join", "for_each", "repeat_until", "iteration_limit", "subflow"]],
  ["Decisions", ["request_input", "approval", "approve_implementation", "quality_gate"]],
  ["Agile", ["link_work", "find_work", "update_progress", "manage_work", "materialize_backlog", "run_backlog"]],
  ["Agents and execution", ["agent", "plan", "review_plan", "implement", "review_candidate", "compare_models", "command", "mcp_tool"]],
  ["GitHub", ["search_public_repositories", "open_or_update_pull_request", "wait_for_ci", "merge_pull_request"]],
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
      !["for_each_begin", "for_each_gate", "for_each_collect", "repeat_output", "delivery_attempt"].includes(kind) && groupFor(kind) === title)
      .map((entry) => ({
        id: entry.kind,
        label: entry.kind === "result" ? "Delivery result" : entry.title,
        input: describePorts(entry.inputs),
        output: describePorts(entry.outputs),
        purpose: nodePurposes[entry.kind],
        definition: entry,
        ...configure(entry),
      })),
  }));
}

function searchCatalogEntry(node: PipelineCatalogEntry, title: string, terms: string[]) {
  const definition = node.definition;
  const effect = definition && effects[definition.effect];
  const text = [title, node.id, node.label, node.input, node.output, node.detail, node.purpose,
    effect?.label, definition?.effect, definition?.configuration, definition?.executor]
    .join(" ").toLowerCase().replaceAll("_", " ");
  return terms.every((term) => text.includes(term));
}

function searchTerms(query: string) {
  return query.trim().toLowerCase().split(/[\s_]+/).filter(Boolean);
}

export function PipelineLibraryCatalog({ groups }: { groups: PipelineCatalogGroup[] }) {
  const searchInput = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState("");
  const [effect, setEffect] = useState("");
  const terms = searchTerms(query);
  const nonEmpty = groups.filter(({ entries }) => entries.length > 0);
  const available = nonEmpty.flatMap(({ entries }) => entries);
  const effectOptions = [...new Set(available.flatMap(({ definition }) => {
    const filter = definition && effects[definition.effect]?.filter;
    return filter ? [filter] : [];
  }))].sort();
  const matches = nonEmpty
    .filter(({ title }) => !category || title === category)
    .map(({ title, entries }) => ({
      title,
      entries: entries.filter((node) => (!effect || (node.definition && effects[node.definition.effect]?.filter === effect)) &&
        searchCatalogEntry(node, title, terms)),
    })).filter(({ entries }) => entries.length > 0);
  const count = matches.reduce((sum, { entries }) => sum + entries.length, 0);
  const filtered = Boolean(query || category || effect);

  function reset() {
    setQuery("");
    setCategory("");
    setEffect("");
    searchInput.current?.focus();
  }

  return <div className="pipeline-library-catalog">
    <div className="pipeline-library-controls" data-has-effects={effectOptions.length > 0 || undefined}>
      <label className="pipeline-library-filter pipeline-library-search">
        <span><Search size={14} aria-hidden /> Search</span>
        <input ref={searchInput} type="search" aria-label="Search pipeline nodes" placeholder="Find a node, port or preset" value={query}
          onChange={(event) => setQuery(event.target.value)} />
      </label>
      <label className="pipeline-library-filter">
        <span><Blocks size={14} aria-hidden /> Category</span>
        <select value={category} onChange={(event) => setCategory(event.target.value)}>
          <option value="">All categories</option>
          {nonEmpty.map(({ title }) => <option key={title}>{title}</option>)}
        </select>
      </label>
      {effectOptions.length > 0 && <label className="pipeline-library-filter">
        <span><Filter size={14} aria-hidden /> Effect</span>
        <select value={effect} onChange={(event) => setEffect(event.target.value)}>
          <option value="">Any effect</option>
          {effectOptions.map((option) => <option key={option}>{option}</option>)}
        </select>
      </label>}
      <div className="pipeline-library-count">
        <p role="status">{filtered ? count + " of " + available.length : count} {available.length === 1 ? "node" : "nodes"}</p>
        {filtered && <Button variant="ghost" type="button" onClick={reset}><X size={14} aria-hidden /> Clear filters</Button>}
      </div>
    </div>
    {count === 0 && <div className="pipeline-library-empty">
      <SearchX size={22} aria-hidden />
      <strong>{filtered ? "No nodes match your filters." : "No nodes available."}</strong>
    </div>}
    {matches.map(({ title, entries }) => {
      const Icon = categoryIcons[title] ?? Blocks;
      return <section className="pipeline-library-category" key={title}>
        <header><h3><Icon size={15} aria-hidden /> {title}</h3><span>{entries.length}</span></header>
        <ul>{entries.map((node) => <li key={node.id}>
          <LibraryCatalogCard node={node} category={title} icon={Icon} />
        </li>)}</ul>
      </section>;
    })}
  </div>;
}

function LibraryCatalogCard({ node, category, icon: Icon }: { node: PipelineCatalogEntry; category: string; icon: LucideIcon }) {
  const purposeId = useId();
  const effect = node.definition && effects[node.definition.effect]?.label;
  return <Dialog.Root>
    <article className="pipeline-library-node">
      <header>
        <h4><Dialog.Trigger asChild><button type="button" data-pipeline-node={node.id}
          aria-describedby={node.purpose ? purposeId : undefined}>{node.label}</button></Dialog.Trigger></h4>
        {node.purpose && <p id={purposeId}>{node.purpose}</p>}
      </header>
      <span className="pipeline-library-node-cue" aria-hidden>View node details <ArrowUpRight size={14} /></span>
    </article>
    <Dialog.Portal>
      <Dialog.Overlay className="pipeline-node-overlay" />
      <Dialog.Content className="pipeline-node-dialog" data-pipeline-node-details={node.id}>
        <header className="pipeline-node-dialog-header">
          <span className="pipeline-node-dialog-icon"><Icon size={20} aria-hidden /></span>
          <div>
            <p className="pipeline-node-dialog-category">{category}{node.simulation && " · Simulation"}</p>
            <Dialog.Title>{node.label}</Dialog.Title>
            <Dialog.Description className="sr-only">Learn how this node works and how to connect it.</Dialog.Description>
          </div>
          <Dialog.Close asChild><Button type="button" variant="ghost" className="pipeline-node-dialog-close" aria-label="Close node details"><X size={18} aria-hidden /></Button></Dialog.Close>
        </header>
        <div className="pipeline-node-dialog-body" role="region" aria-label="Node documentation" tabIndex={0}>
          {node.details ?? <PipelineNodeDocumentation purpose={node.purpose} definition={node.definition}
            simulation={node.simulation} effect={effect} />}
        </div>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
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
