import type { ModelSelection } from "@/bindings/ModelSelection";
import type { SchemaField, SchemaValue } from "@/workflow-schema-model";
import type { RoleSnapshot } from "@/workflow-typed-prompt";

const string: SchemaValue = { type: "string" };
export const boolean: SchemaValue = { type: "boolean" };
export const integer: SchemaValue = { type: "integer" };
export const field = (name: string, value = string, required = true): SchemaField => ({ name, ...value, required });
export const object = (fields: SchemaField[]): SchemaValue => ({ type: "object", fields });
export const list = (items = string): SchemaValue => ({ type: "array", items });
const strings = (...names: string[]) => names.map((name) => field(name, list()));
const text = (...names: string[]) => names.map((name) => field(name));

const task = object([
  ...text("key", "title", "description", "kind"), field("priority", integer),
  ...strings("depends_on", "requirement_ids", "scenario_ids", "acceptance_criteria"),
]);

const check = object([
  ...text("id", "status", "evidence"), field("exit_code", integer, false),
]);

export const developmentSchemas = {
  "Feature Request": [
    field("request"), field("goals", list(), false), field("constraints", list(), false),
    field("context", string, false), field("destination_branch", string, false), field("remote", string, false),
  ],
  "Discovery Report": [field("summary"), field("findings", list(object(text("claim", "source")))), ...strings("questions", "assumptions")],
  "Master Plan": [field("summary"), ...strings("scope", "components", "dependencies", "decisions", "risks")],
  "SDD Specification": [
    field("requirements", list(object([...text("id", "behavior"), ...strings("acceptance_criteria", "invariants")]))),
    field("interfaces", list(object([...text("id", "behavior"), ...strings("constraints")]))),
  ],
  "BDD Specification": [field("scenarios", list(object([...text("id", "kind"), ...strings("requirement_ids", "given", "when", "then")])))],
  "Technical Design": [
    field("components", list(object([...text("id", "change"), ...strings("interfaces", "requirement_ids")]))),
    ...strings("paths", "risks"),
  ],
  "Test Strategy": [
    field("checks", list(object([
      ...text("id", "category", "command"), field("required", boolean), ...strings("requirement_ids", "scenario_ids"),
      field("omission_reason", string, false),
    ]))),
    ...strings("allowed_test_paths", "release_criteria"),
  ],
  "Agile Backlog": [field("tasks", list(task))],
  "Review Report": [
    ...text("subject", "dimension", "verdict", "risk"),
    field("findings", list(object(text("severity", "subject_id", "evidence", "recommendation")))),
  ],
  "Execution Report": [
    ...text("task_key", "base_sha", "status"),
    ...["tree", "worktree_path", "head_sha", "integration_sha", "pr_url", "diagnostic"].map((name) => field(name, string, false)),
    field("checks", list(check)),
  ],
  "Delivery Round": [
    field("tasks", list(object([
      ...text("key", "status"), field("attempt", integer),
      ...["head_sha", "integration_sha", "diagnostic"].map((name) => field(name, string, false)),
    ]))),
    field("stop", boolean), field("status"),
  ],
  "Release Report": [
    field("status"), field("integrated_sha", string, false),
    field("deliveries", list(object(text("task_key", "pr_url", "integration_sha")))), ...strings("evidence", "blockers"),
  ],
};

const roles = {
  discovery: ["Discovery Analyst", "read_only", "Inspect the supplied sources and cite facts, reusable components, constraints, and open questions."],
  specification: ["Specification Author", "read_only", "Write decision-complete artifacts with stable IDs and testable, traceable acceptance criteria."],
  review: ["Independent Reviewer", "read_only", "Review the exact supplied revision through the assigned lens. Explain each blocking finding and cite evidence."],
  backlog: ["Backlog Coordinator", "read_only", "Preserve task identities and prerequisites. Select ready work by priority and record verified outcomes."],
  implementation: ["Implementation Engineer", "approved_write", "Work in the authorized task worktree. Write focused tests, implement the task, and preserve traceability."],
  verification: ["Verification Engineer", "approved_write", "Run the approved commands on the specified Git tree. Record exit codes, test IDs, and diagnostics."],
  delivery: ["Delivery Engineer", "approved_write", "Operate on the authorized repository and branch. Verify the candidate, required checks, and integration revision."],
} as const;

export type DevelopmentRole = keyof typeof roles;
export function developmentRole(key: DevelopmentRole, model: ModelSelection): RoleSnapshot {
  const [name, toolScope, instructions] = roles[key];
  return {
    name, role: "agent", model, effort: null, toolScope, reviewPolicy: "independent", instructions,
    completionContract: "Return one JSON object matching the Output Schema. Cite the commands, source paths, Git revisions, or check results that support your findings.",
  };
}

export const developmentRoles = Object.keys(roles) as DevelopmentRole[];

export const developmentPrompts = {
  repository: "Inspect repository entry points, conventions, dependencies, and reuse opportunities. Cite paths and symbols. Identify the selected Git remote and destination branch, using the explicit launch values or the repository's remote and its configured default branch. Record unresolved delivery settings as blocking questions.",
  research: "Compare relevant public implementations, licenses, and tradeoffs. Cite sources and record any research limitations.",
  requirements: "Extract goals, constraints, and unresolved decisions. List questions that block planning.",
  masterPlan: "Define scope, architecture, dependencies, and risk decisions. Address the previous blocking review findings and preserve the approved constraints.",
  sdd: "Specify canonical requirements, interfaces, invariants, and acceptance criteria. Assign stable requirement IDs.",
  bdd: "Derive traceable Given–When–Then scenarios, including negative and edge cases.",
  design: "Specify component boundaries, data structures, interfaces, algorithms, and failure handling.",
  testStrategy: "Map the requirements to executable checks found in the project and explain which checks apply. Cover unit, integration, end-to-end, functional, mutation, static analysis, dependency, security, performance, and regression checks as applicable.",
  consistency: "Cross-check the artifacts for contradictions, missing dependencies, and broken traceability. Identify the artifacts and review findings affected by each proposed correction.",
  backlog: "Create atomic work items with distinct keys, priorities, prerequisites, and observable acceptance criteria. Link each task to the exact requirements and scenarios it implements.",
  dispatch: "Select the highest-priority ready tasks, breaking ties by task key. A prerequisite is satisfied only by a recorded merge. Preserve work already pending checks, task identity, and attempt count, and allocate independent work to separate slots.",
  red: "Use the authorized worktree. Fetch the configured destination and verify that the candidate starts from its current revision. Preserve existing task changes when updating the base and revalidate Red after a base change. Run the baseline, add focused tests, and reproduce each expected assertion failure before changing production code. Record the absolute worktree path, base SHA, test IDs, changed test paths, commands, and failure reasons. Report a blocker for infrastructure failures or unexpected results.",
  implement: "Implement the assigned task in the authorized worktree and address the supplied failure evidence. Refactor and run focused checks. Report its absolute worktree_path, base_sha, and exact Git tree for independent validation.",
  verify: "Run the assigned checks on an isolated copy of the exact candidate tree. Record observed exit codes, test IDs, and evidence locations. Report missing, failed, or inapplicable checks with their reasons.",
  buildAndPull: "Build the verified candidate before creating its scoped atomic commit. Include the task key and requirement IDs in the commit. Confirm the resulting tree. Find or update the existing pull request by its task branch and head, and record the actual head and pull request URL. Return retry on a failed build before publishing the candidate.",
  ci: "Read each required check for the exact pull request head. Record the pull request, head, base, and pending, failed, or passed status. Treat an unknown risk classification as high risk.",
  merge: "Verify the current head, base, required checks, and applicable approval. Reuse the merge proof if this head is already integrated. Otherwise use the normal repository merge policy and gh pr merge with --match-head-commit for the approved head. Confirm the integration SHA in the remote before completing the task. Return retry for stale references and blocked for an uncertain effect.",
  audit: "Audit the integrated SHA against the approved specifications. Record the full regression result and propose scoped, traceable remediation for each blocker.",
  documentation: "Update the affected documentation and verify its examples. Clean up only temporary directories owned by this run, after confirming their work is integrated.",
  release: "Verify the destination SHA, regression, final build, required checks, and approvals. Report ready only when every required result is supported by evidence on the final SHA.",
};

export const developmentReview = (artifact: string, dimension: string) =>
  `Review ${artifact} for ${dimension}. Cite the exact evidence and propose a correction for every blocking finding.`;
