import type { ModelSelection } from "@/bindings/ModelSelection";
import type { ReusableBody, ReusableReference } from "@/workflow-reusable";
import type { WorkflowEndpoint, WorkflowGraph, WorkflowNodeConfiguration } from "@/workflow-graph-model";
import type { SchemaField } from "@/workflow-schema-model";
import {
  boolean, developmentPrompts as prompts, developmentReview, developmentRole, developmentRoleDefinition, developmentRoleName, developmentRoles,
  developmentSchemas as schemas, field, integer, list, object, type DevelopmentRole,
} from "@/development-pipeline-content";

type Fields = SchemaField[];
type Flow = { ready: WorkflowEndpoint; data: WorkflowEndpoint; fields: Fields };
type Member = { key: string; name: string; body: ReusableBody; references?: Record<string, ReusableReference> };
type Graph = { graph: WorkflowGraph; references: Record<string, ReusableReference> };
type Branches = Record<string, (input: Flow) => Flow>;

export const developmentPipelineName = "Development Pipeline";
const endpoint = (node: string, port: string): WorkflowEndpoint => ({ node, port });
const draft = (key: string): ReusableReference => ({ source: "draft", key });
const slug = (title: string) => title.toLowerCase().replace(/[^a-z0-9]+/g, "-");
const newGraph = (name: string): Graph => ({ graph: { name, nodes: [], wires: [] }, references: {} });
const flow = (id: string, fields: Fields, ready = "completed", data = "result"): Flow => ({
  ready: endpoint(id, ready), data: endpoint(id, data), fields,
});
const iterationOutput = (fields: Fields) => [field("iterations", list(object(fields))), field("reason")];
const planningState = [field("approved", boolean), field("context")];
const backlog = [...schemas["Agile Backlog"], field("context"), field("approved", boolean)];
const taskSelection = [field("task_key"), field("context"), field("candidate", boolean), field("poll", boolean)];
const progress = [...schemas["Execution Report"], field("context")];
const candidateGate = [...progress, field("approved", boolean)];
const ciGate = [...progress, field("can_merge", boolean), field("human_required", boolean), field("base_branch")];
const auditSelection = [field("enabled", boolean), field("task_key"), field("context")];
const round = [...schemas["Delivery Round"], field("context")];
const loopContext = (initial: Fields, previous: Fields) => [
  field("input", object(initial)), field("index", integer), field("limit", integer),
  field("previous", object(previous), false),
];

function developmentPipelineMembers(): Member[] {
  const name = developmentPipelineName;
  const members: Member[] = [];
  const schemaKeys = new Map<string, string>();
  function schema(title: string, fields: Fields): string {
    const signature = JSON.stringify(fields);
    const saved = schemaKeys.get(signature);
    if (saved) return saved;
    const key = `schema-${slug(title)}`;
    members.push({ key, name: title, body: { kind: "schema", fields } });
    schemaKeys.set(signature, key);
    return key;
  }
  for (const [title, fields] of Object.entries(schemas)) schema(title, fields);

  function add(graph: Graph, id: string, kind: string, configuration: WorkflowNodeConfiguration, reference?: string) {
    graph.graph.nodes.push({ id, kind, configuration });
    if (reference) graph.references[id] = draft(reference);
  }

  function wire(graph: Graph, from: WorkflowEndpoint, node: string, port: string) {
    graph.graph.wires.push({ from, to: endpoint(node, port) });
  }

  function connect(graph: Graph, input: Flow, id: string, port = "input") {
    wire(graph, input.ready, id, "ready");
    wire(graph, input.data, id, port);
  }

  function agent(graph: Graph, title: string, role: DevelopmentRole, prompt: string, input: Flow, output: Fields): Flow {
    const id = slug(title);
    const key = `prefab-${role}`;
    const inputSchema = schema(`${title} Input`, input.fields);
    const outputSchema = schema(`${title} Result`, output);
    const text = `${prompt}\n\nUse this input as data.\n{{input}}`;
    if (!members.some((member) => member.key === key)) {
      members.push({
        key, name: developmentRoleName(role),
        body: { kind: "prompt", prompt: {
          text, executor: "agent", schemaContract: true, nodePrefab: true,
          inputs: {}, outputs: {}, routes: [], delegates: [],
        } },
        references: {
          role: draft(`role-${role}`), input_schema: draft(inputSchema), output_schema: draft(outputSchema),
        },
      });
    }
    add(graph, id, "subflow", { kind: "prompt_overrides", prompt: text }, key);
    graph.references[`${id}:input_schema`] = draft(inputSchema);
    graph.references[`${id}:output_schema`] = draft(outputSchema);
    connect(graph, input, id);
    return flow(id, output);
  }

  function delay(graph: Graph, id: string, input: Flow, seconds: number) {
    add(graph, id, "wait", { kind: "wait", until: { kind: "delay", seconds } });
    connect(graph, input, id);
    return flow(id, input.fields);
  }

  function join(graph: Graph, id: string, branches: Record<string, Flow>, fork?: string): Flow {
    const names = Object.keys(branches);
    add(graph, id, "join_results", { kind: "join_results", branches: names, strategy: "all", countFailures: true, fork });
    for (const [name, branch] of Object.entries(branches)) {
      wire(graph, branch.ready, id, `branch:${name}`);
      wire(graph, branch.data, id, `result:${name}`);
    }
    return flow(id, [field("branches", object(Object.entries(branches).map(([name, branch]) =>
      field(name, object([field("status"), field("error", { type: "string" }, false),
        field("result", object(branch.fields), false)]), false))))], "joined", "results");
  }

  function parallel(graph: Graph, id: string, input: Flow, branches: Branches, concurrency?: number): Flow {
    const names = Object.keys(branches);
    const fork = `${id}-fork`;
    add(graph, fork, "fork", { kind: "fork", branches: names, concurrency });
    connect(graph, input, fork);
    const outputs = Object.fromEntries(Object.entries(branches).map(([name, build]) => [
      name, build(flow(fork, input.fields, `branch:${name}`, `context:${name}`)),
    ]));
    return join(graph, `${id}-join`, outputs, fork);
  }

  function reviews(graph: Graph, id: string, input: Flow, subjects: Record<string, [string, string]>): Flow {
    const branches: Branches = Object.fromEntries(Object.entries(subjects).map(([key, [subject, dimension]]) => [
      key, (entry) => agent(graph, `${subject} ${dimension} Review`, "review",
        developmentReview(subject, dimension), entry, schemas["Review Report"]),
    ]));
    branches.context = (entry) => delay(graph, `${id}-context`, entry, 0);
    return parallel(graph, id, input, branches);
  }

  function human(graph: Graph, id: string, input: Flow, message: string, writes = false): Flow {
    add(graph, id, "human", { kind: "human", interaction: "approval", message, fields: writes
      ? [{ id: "authorizeWrites", label: "Authorize writes to managed task worktrees", required: true, input: { kind: "boolean" } }]
      : [] });
    connect(graph, input, id);
    return flow(id, input.fields, "approved", "approved_result");
  }

  function iteration(
    parent: Graph, title: string, initial: Flow, result: Fields, maxIterations: number,
    test: { exitWhen?: { op: "equals"; pointer: string; value: boolean }; breakWhen?: { op: "equals"; pointer: string; value: boolean } },
    body: (graph: Graph, entry: Flow) => Flow,
    limit: "fail" | "complete" | "limit_reached" = "fail",
  ): Flow {
    const id = slug(title);
    const key = `function-${id}`;
    const graph = newGraph(title);
    const context = loopContext(initial.fields, result);
    add(graph, "start", "start", { kind: "subflow_entry", outputs: [
      { name: "ready", value: { kind: "control" } },
      { name: "inputs", value: { kind: "data", artifact: "json_object" } },
    ] });
    graph.references["start:schema"] = draft(schema(`${title} Context`, context));
    const output = body(graph, flow("start", context, "ready", "inputs"));
    members.push({
      key, name: title, references: graph.references,
      body: { kind: "subflow", graph: graph.graph, inputs: {
        ready: endpoint("start", "ready"), context: endpoint("start", "inputs"),
      }, outputs: { completed: output.ready, result: output.data } },
    });
    add(parent, id, "loop", { kind: "loop", maxIterations, limit, readyInput: "ready", contextInput: "context",
      completionOutput: "completed", resultOutput: "result", ...test }, key);
    connect(parent, initial, id);
    return flow(id, iterationOutput(result), "completed", "results");
  }

  function subflow(parent: Graph, id: string, title: string, input: Flow, output: Fields,
    body: (graph: Graph, entry: Flow) => Flow): Flow {
    const key = `function-${slug(title)}`;
    if (!members.some((member) => member.key === key)) {
      const graph = newGraph(title);
      add(graph, "start", "start", { kind: "subflow_entry", outputs: [
        { name: "ready", value: { kind: "control" } },
        { name: "inputs", value: { kind: "data", artifact: "json_object" } },
      ] });
      graph.references["start:schema"] = draft(schema(`${title} Input`, input.fields));
      const result = body(graph, flow("start", input.fields, "ready", "inputs"));
      members.push({ key, name: title, references: graph.references, body: {
        kind: "subflow", graph: graph.graph,
        inputs: { ready: endpoint("start", "ready"), input: endpoint("start", "inputs") },
        outputs: { completed: result.ready, result: result.data },
      } });
    }
    add(parent, id, "subflow", { kind: "none" }, key);
    connect(parent, input, id);
    return flow(id, output);
  }

  function optional(parent: Graph, title: string, input: Flow, result: Fields, flag: string,
    body: (graph: Graph, entry: Flow) => Flow, skip = false) {
    return iteration(parent, title, input, result, 1, {
      breakWhen: { op: "equals", pointer: `/input/${flag}`, value: skip },
    }, body, "complete");
  }

  function taskSlot(graph: Graph, entry: Flow): Flow {
    const reviewCandidate = (dimension: string) => `${developmentReview("the frozen candidate", dimension)} Use the supplied absolute worktree_path with git -C. Compare the supplied tree against base_sha and report missing or mismatched evidence as blocking.`;
    return optional(graph, "Candidate Attempt", entry, progress, "candidate", (current, context) => {
        const tests = agent(current, "Baseline and Red", "implementation",
          `${prompts.red} For documentation-only tasks record applicable document checks. On retries reuse the prior Red only after verifying the same base and test tree. Include task instructions and test IDs in context. Set approved only when Red is verified.`,
          context, candidateGate);
        const red = agent(current, "Red Evidence Gate", "review",
          "Confirm that the recorded baseline passed and each new test failed on the expected assertion. Inspect the supplied absolute worktree_path with git -C and verify the approved test paths and exact test tree. Treat compilation, setup, and missing results as blockers. Preserve worktree_path, task scope, and evidence and set approved only for a valid Red or documented documentation checks.",
          tests, candidateGate);
        const result = parallel(current, "red-gate", red, {
          red: (input) => delay(current, "red-evidence", input, 0),
          implementation: (input) => optional(current, "Implement Candidate", input, progress, "approved", (implementation, redContext) => {
            const candidate = agent(implementation, "Implement or Fix", "implementation",
              `${prompts.implement} For documentation explicitly apply these instructions. ${prompts.documentation} Run Green, stage the intended task files, and record the tree from git write-tree and command results. Leave the task ready for independent review. Carry the task, spec, Red and test strategy in compact context.`, redContext, progress);
            const checks = parallel(implementation, "independent-validation", candidate, {
              correctness: (entry) => agent(implementation, "Code Correctness Review", "review", reviewCandidate("correctness"), entry, schemas["Review Report"]),
              architecture: (entry) => agent(implementation, "Code Architecture Review", "review", reviewCandidate("architecture"), entry, schemas["Review Report"]),
              simplification: (entry) => agent(implementation, "Code Simplification Review", "review", reviewCandidate("simplicity"), entry, schemas["Review Report"]),
              efficiency: (entry) => agent(implementation, "Code Efficiency Review", "review", reviewCandidate("efficiency"), entry, schemas["Review Report"]),
              security: (entry) => agent(implementation, "Code Security Review", "review", reviewCandidate("security and mitigations"), entry, schemas["Review Report"]),
              qa: (entry) => agent(implementation, "Independent QA and Automated Checks", "verification",
                `${prompts.verify} Run the unit, integration, end-to-end, functional, mutation, static, dependency and regression groups concurrently when their outputs are isolated.`, entry, progress),
              candidate: (entry) => delay(implementation, "capture-candidate", entry, 0),
            });
            const quality = agent(implementation, "Quality Gate", "review",
              "Require exact-tree Green, each independent code review, and all applicable recorded checks. Validate the tree using Git and examine evidence for every required result. Preserve the candidate and risk classification in context. Set approved only when all mandatory evidence passes.",
              checks, candidateGate);
            const built = parallel(implementation, "build-gate", quality, {
              gate: (entry) => delay(implementation, "quality-evidence", entry, 0),
              build: (entry) => optional(implementation, "Publish Candidate", entry, progress, "approved",
                (build, input) => agent(build, "Build Commit and Open Pull Request", "delivery", prompts.buildAndPull, input, progress)),
            });
            return agent(implementation, "Record Candidate Outcome", "backlog",
              "Return the recorded build and exact pull request evidence when available. Return retry with the gate or build diagnostics on failure. Preserve the task key and attempt context.", built, progress);
          }),
        });
        return agent(current, "Record Implementation Attempt", "backlog",
          "Return the implementation result when the Red gate passed. Otherwise return retry or blocked with the actual Red diagnostic. Preserve task key and context.", result, progress);
    });
  }

  function ciSlot(graph: Graph, entry: Flow): Flow {
    return optional(graph, "CI and Merge", entry, progress, "poll", (current, context) => {
        const waiting = delay(current, "wait-for-ci", context, 60);
        const inspected = agent(current, "Inspect Pull Request Checks", "backlog",
          `${prompts.ci} Preserve prior task context and set can_merge only for passing checks at the expected head and base. Set human_required for high or unknown risk or a project policy requiring manual approval.`,
          waiting, ciGate);
        const outcome = parallel(current, "ci-gate", inspected, {
          checks: (entry) => delay(current, "ci-evidence", entry, 0),
          merge: (entry) => optional(current, "Merge Passing Candidate", entry, progress, "can_merge", (approved, result) => {
            const request = agent(approved, "Prepare Merge Authorization", "backlog",
              "Copy the exact pull request, head, base, observed checks and human_required from the input. Preserve task_key and context.", result, ciGate);
            // Connect each merge route only to its corresponding proof.
            const mergePaths = parallel(approved, "merge-approval", request, {
              automatic: (entry) => optional(approved, "Merge Low Risk Candidate", entry, progress, "human_required",
                (merge, input) => agent(merge, "Integrate Low Risk Candidate", "delivery", prompts.merge, input, progress), true),
              human: (entry) => optional(approved, "Approve High Risk Merge", entry, progress, "human_required", (merge, input) => {
                const candidate = agent(merge, "Show Exact Merge Revision", "backlog",
                  "Expose the exact reviewed pull request URL, head, base, and check results for the operator. Copy task scope from input.", input, ciGate);
                const confirmed = human(merge, "approve-merge", candidate, "Approve merging this exact pull request revision.");
                return agent(merge, "Integrate Approved Candidate", "delivery", prompts.merge, confirmed, progress);
              }),
            });
            return agent(approved, "Record Merge Outcome", "backlog",
              "Use the one selected merge result. Record completion only with the verified integration SHA. Record a blocker for rejection, missing evidence, or an uncertain merge result.", mergePaths, progress);
          }),
        });
        return agent(current, "Record CI Outcome", "backlog",
          "Return pending, retry, blocked, or merged using the check and integration evidence. Preserve task identity and the current pull request head.", outcome, progress);
    });
  }

  const root = newGraph(name);
  add(root, "start", "typed_start", { kind: "typed_start", fields: [] });
  root.references["start:schema"] = draft(schema("Feature Request", schemas["Feature Request"]));
  const request = flow("start", schemas["Feature Request"], "start", "inputs");
  const discovery = parallel(root, "discovery", request, {
    repository: (input) => agent(root, "Repository Analysis", "discovery", prompts.repository, input, schemas["Discovery Report"]),
    research: (input) => agent(root, "Open Source Research", "discovery", prompts.research, input, schemas["Discovery Report"]),
    requirements: (input) => agent(root, "Requirements Analysis", "discovery", prompts.requirements, input, schemas["Discovery Report"]),
    request: (input) => delay(root, "discovery-request", input, 0),
  });
  const discoveryState = [field("context"), field("needs_clarification", boolean), field("questions", list())];
  const discovered = agent(root, "Synthesize Discovery", "discovery",
    "Preserve the request and independent findings as compact JSON text in context. Set needs_clarification when a blocking question remains.", discovery, discoveryState);
  const clarifiedFields = [field("context")];
  const consolidated = parallel(root, "requirements-input", discovered, {
    original: (input) => delay(root, "original-discovery", input, 0),
    clarification: (input) => iteration(root, "Clarify Requirements", input, clarifiedFields, 1, {
      breakWhen: { op: "equals", pointer: "/input/needs_clarification", value: false },
    }, (graph, entry) => {
      add(graph, "question", "human", { kind: "human", interaction: "input",
        message: "Resolve the blocking requirements questions.",
        fields: [{ id: "answers", label: "Answers", required: true, input: { kind: "text" } }] });
      connect(graph, entry, "question");
      const answer = flow("question", [field("interaction"), field("status"),
        field("original", object(entry.fields), false), field("values", object([field("answers")]))], "answered", "response");
      return agent(graph, "Apply Clarification", "discovery",
        "Combine the original discovery context and the human answers. Preserve the complete clarified scope as compact JSON text.", answer, clarifiedFields);
    }, "complete"),
  });
  const accepted = agent(root, "Prepare Planning Context", "specification",
    "Extract the complete request and discovery context. Use the human answers when a clarification round ran. Preserve the scope as compact JSON text.", consolidated, planningState);
  const approval = { op: "equals" as const, pointer: "/result/approved", value: true };
  const master = iteration(root, "Master Plan Round", accepted, planningState, 3, { exitWhen: approval }, (graph, entry) => {
    const plan = agent(graph, "Draft Master Plan", "specification", prompts.masterPlan, entry, schemas["Master Plan"]);
    const candidate = agent(graph, "Assemble Master Plan Snapshot", "specification",
      "Preserve the exact master plan and its discovery context as compact JSON text in context.",
      join(graph, "master-context", { plan, context: entry }), planningState);
    const reviewed = reviews(graph, "master-reviews", candidate, {
      correctness: ["Master Plan", "Correctness"], architecture: ["Master Plan", "Architecture"], feasibility: ["Master Plan", "Feasibility"],
    });
    return agent(graph, "Master Plan Gate", "review",
      "Require three successful approving reviews. Preserve the exact candidate, discovery context, and review findings in compact JSON text. Set approved only when every blocking finding is resolved.",
      reviewed, planningState);
  });
  const specification = iteration(root, "Specification Round", master, planningState, 3, { exitWhen: approval }, (graph, entry) => {
    const sdd = agent(graph, "Write SDD", "specification", `${prompts.sdd} Reuse approved material from previous when its inputs match.`, entry, schemas["SDD Specification"]);
    const source = agent(graph, "Prepare Specification Inputs", "specification",
      "Preserve the exact SDD, approved master plan, and the prior round's findings as compact JSON text in context.",
      join(graph, "sdd-context", { sdd, context: entry }), planningState);
    const detailed = parallel(graph, "detail", source, {
      bdd: (input) => agent(graph, "Write BDD", "specification", prompts.bdd, input, schemas["BDD Specification"]),
      design: (input) => agent(graph, "Write Technical Design", "specification", prompts.design, input, schemas["Technical Design"]),
      strategy: (input) => agent(graph, "Write Test Strategy", "specification", prompts.testStrategy, input, schemas["Test Strategy"]),
      source: (input) => delay(graph, "specification-source", input, 0),
    });
    const snapshot = agent(graph, "Assemble Specification Snapshot", "specification",
      "Assemble the request, master plan, SDD, BDD, design, and test strategy in context as compact JSON text. Preserve each document and its IDs. Record any missing result.", detailed, planningState);
    const reviewed = reviews(graph, "specification-reviews", snapshot, {
      sdd: ["SDD", "Coverage"], bdd: ["BDD", "Scenarios"], design: ["Technical Design", "Feasibility"],
      strategy: ["Test Strategy", "Effectiveness"],
    });
    return agent(graph, "Global Consistency Gate", "review",
      `${prompts.consistency} Preserve the candidate and findings in context. Require each review and a consistent complete specification before setting approved.`,
      reviewed, planningState);
  });
  const planned = iteration(root, "Backlog Round", specification, backlog, 3, { exitWhen: approval }, (graph, entry) => {
    const tasks = agent(graph, "Generate Agile Backlog", "backlog",
      `${prompts.backlog} Include a documentation task with key documentation that depends on all implementation tasks. Include reserved entries release-checks, remediation-1, and remediation-2 with kinds audit, remediation, and remediation. Restrict remediation to findings within the approved specifications. Release checks operate only on isolated test output.`, entry, schemas["Agile Backlog"]);
    const planning = join(graph, "backlog-context", { tasks, context: entry });
    const candidate = agent(graph, "Assemble Backlog Snapshot", "backlog",
      "Copy the generated tasks exactly and preserve the latest complete specification in context as compact JSON text.", planning, backlog);
    const reviewed = reviews(graph, "backlog-reviews", candidate, {
      completeness: ["Agile Backlog", "Completeness"], dependencies: ["Agile Backlog", "Dependencies"],
      granularity: ["Agile Backlog", "Granularity"], testability: ["Agile Backlog", "Testability"],
    });
    return agent(graph, "Backlog Gate", "backlog",
      "Check the task graph for cycles, duplicate keys, unknown dependencies, missing requirements and scenarios, and absent acceptance criteria. Preserve the backlog and specification. Approve only if the four independent reviews passed and every reference resolves.",
      reviewed, backlog);
  });
  const snapshot = agent(root, "Prepare Approval Snapshot", "specification",
    "Copy the last approved iteration's tasks and context into the result exactly.", planned, backlog);
  const approved = human(root, "approve-work", snapshot, "Approve this reviewed plan and backlog for implementation.", true);

  const dispatchPlan = [field("context"), field("slot1", object(taskSelection)), field("slot2", object(taskSelection)),
    field("ci", object(taskSelection)), field("audit", object(auditSelection))];
  const ran = iteration(root, "Delivery and Validation Round", approved, round, 24, {
    exitWhen: { op: "equals", pointer: "/result/stop", value: true },
  }, (graph, entry) => {
    const dispatch = agent(graph, "Dispatch Backlog Work", "backlog",
      `${prompts.dispatch} Slots slot1 and slot2 are for candidate attempts. Use the single ci slot to poll and merge the next pending pull request. Use at most two distinct active tasks total and three attempts per task. Never assign the same task to multiple slots. Use an empty task key and false flags for idle slots. Read the last round's context for delivery evidence, retries and CI counts. Set audit.enabled only when no delivery is active. Reserve remediation and release-checks entries for audits. Schedule documentation only after the first clean audit. Preserve the approved specification and state as compact JSON text in context.`,
      entry, dispatchPlan);
    const settled = parallel(graph, "round-work", dispatch, {
      slot1: (input) => {
        const selected = agent(graph, "Select First Task", "backlog", "Copy slot1 exactly and retain the approved task instructions in its context.", input, taskSelection);
        return subflow(graph, "first-task", "Task Delivery Slot", selected, iterationOutput(progress), taskSlot);
      },
      slot2: (input) => {
        const selected = agent(graph, "Select Second Task", "backlog", "Copy slot2 exactly and retain the approved task instructions in its context.", input, taskSelection);
        return subflow(graph, "second-task", "Task Delivery Slot", selected, iterationOutput(progress), taskSlot);
      },
      ci: (input) => {
        const selected = agent(graph, "Select CI Task", "backlog", "Copy ci exactly and retain its pull request and approved task instructions in context.", input, taskSelection);
        return ciSlot(graph, selected);
      },
      audit: (input) => {
        const scope = agent(graph, "Select Integrated Audit", "backlog",
          "Copy audit exactly. Use release-checks as task_key and preserve the actual integrated head and complete scope in context.", input, auditSelection);
        return optional(graph, "Integrated Audit", scope, round, "enabled", (audit, context) => {
          const regression = agent(audit, "Integrated Regression and Build", "verification",
            "Fetch the configured destination, pin its exact SHA, and run the complete regression and release build on an isolated copy. Retain specs, delivery evidence, documentation state, and check output in context.",
            context, progress);
          const reviewed = reviews(audit, "final-audit", regression, {
            requirements: ["Integrated Requirements", "Coverage"], bdd: ["Integrated BDD", "Acceptance"],
            architecture: ["Integrated Architecture", "Consistency"], quality: ["Integrated Quality", "Release Readiness"],
          });
          return agent(audit, "Integrated Audit Outcome", "backlog",
            `${prompts.audit} Activate at most two reserved remediation passes for in-scope blockers and record their exact tasks in context. Report blocked on scope expansion or exhausted attempts. Request the documentation delivery on the first clean audit. Recheck the final SHA and report ready only after documentation is integrated and every required check passes.`,
            reviewed, round);
        });
      },
      context: (input) => delay(graph, "round-context", input, 0),
    }, 2);
    return agent(graph, "Settle Delivery Round", "backlog",
      "Combine the saved dispatch context, task results, and the integrated audit outcome. Mark tasks merged only with exact integration evidence. Count retries and at most ten CI polls per head. Keep context concise and preserve task keys, current check references, and actionable failures. The next round reads the immutable plan separately. Set stop for ready, blocked or cancelled. Finish with blocked if the round limit is reached.",
      settled, round);
  }, "complete");
  const report = agent(root, "Development Release Report", "backlog",
    `${prompts.release} Use the last delivery round. Include integrated tasks, evidence and blockers. An exhausted run with outstanding work is blocked.`, ran, schemas["Release Report"]);
  add(root, "end", "end", { kind: "end" });
  root.references["end:schema"] = draft(schema("Release Report", schemas["Release Report"]));
  connect(root, report, "end", "result");
  members.push({ key: "pipeline", name, body: { kind: "pipeline", graph: root.graph }, references: root.references });
  return members;
}

export function developmentPipelineCatalog() {
  return [
    ...developmentRoles.map((key) => ({
      key: `role-${key}`, name: developmentRoleName(key), kind: "role", body: { kind: "role", template: developmentRoleDefinition(key) },
    })),
    ...developmentPipelineMembers().map((member) => ({
      ...member, kind: member.body.kind === "prompt" && member.body.prompt?.nodePrefab ? "prefab" : member.body.kind,
    })),
  ];
}

export function developmentPipelinePackage(model: ModelSelection): { name: string; members: Member[] } {
  const roles = developmentRoles.map((key) => ({
    key: `role-${key}`, name: developmentRoleName(key),
    body: { kind: "role", template: developmentRole(key, model) },
  }));
  return { name: developmentPipelineName, members: [...roles, ...developmentPipelineMembers()] };
}
