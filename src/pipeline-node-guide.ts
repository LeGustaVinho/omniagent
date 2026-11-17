import { MAX_ITERATION_ITEMS } from "@/workflow-graph-model";

type Parameter = readonly [field: string, name: string, meaning: string];
type Example = readonly [title: string, steps: string];

export const nodeParameters: Partial<Record<string, readonly Parameter[]>> = {
  validate_output: [["schema", "JSON schema", "Define the expected object. Include referenced schema fragments in the same document."]],
  quality_gate: [["rules", "Decision rules", "Name each rule and choose the evidence field and condition it evaluates."]],
  run_backlog: [
    ["child", "Child workflow", "Choose the exact published workflow revision to run for each eligible task."],
    ["maxParallel", "Parallel tasks", "Set how many child workflows may run in parallel. Queue capacity still applies."],
    ["failed", "Failure port", "Choose the child output that signals a failed delivery."],
  ],
  verify_test_phase: [
    ["phase", "Test phase", "Use Baseline for the base tree, Red for an expected failure with the test patch, or Green for the implemented candidate."],
    ["check", "Registered check", "Select the project check that produces the phase evidence."],
    ["allowedTestPaths", "Test-only paths", "For Red, restrict changed files to the directories, paths, or file suffixes used by the tests."],
  ],
  verify_integrated_checks: [["checks", "Registered checks", "Choose the project checks to run on the pinned integrated revision."]],
  search_public_repositories: [["limit", "Result limit", "Cap the number of public repositories recorded for the query."]],
  typed_start: [
    ["fields", "Launch inputs", "Declare each field by name and type. Conversation runs can fill a Text field from the message or an Array field from earlier messages."],
  ],
  load_work_context: [
    ["mode", "Context source", "Read the task description and checklist, a saved Markdown revision, or both."],
  ],
  manage_work: [
    ["action", "Work action", "Link an existing task, create a task from a title, or create a sub-task from a title and parent task."],
  ],
  find_work: [
    ["query", "Work filters and results", "Filter by work type, title or checklist text, status and parent where applicable. Exactly one match also exposes the task and progress target."],
  ],
  update_progress: [
    ["action", "Progress action", "Check or reopen a checklist item, or select the new work status. Connect a target carrying the expected revision."],
  ],
  set_fields: [
    ["mode", "Object source", "Build a new object or start from the connected source object."],
    ["operations", "Field operations", "Set a destination path from a fixed value or source field, or rename a field. Paths use JSON Pointer."],
  ],
  filter_map_list: [
    ["predicate", "Item condition", "Optionally keep items by testing whether a field exists or equals a value."],
    ["mappings", "Field mappings", "Use source and destination JSON Pointers to copy fields. Leave empty to retain the original shape."],
  ],
  build_text: [
    ["template", "Markdown template", "Insert fields from the Values input with {{field_name}} placeholders."],
  ],
  build_list: [
    ["source", "List source", "Use a configured list or read an array from a connected artifact."],
    ["values", "List items", "Provide up to " + MAX_ITERATION_ITEMS + " items when the source is Configured items."],
    ["pointer", "Array path", "For a source artifact, identify the array with a JSON Pointer."],
  ],
  request_input: [
    ["message", "Question message", "Explain what the operator needs to provide."],
    ["fields", "Answer fields", "Give each answer an ID, label, type and required setting. Choices can allow multiple answers."],
  ],
  approval: [
    ["message", "Approval message", "Describe the decision and the scope the operator is reviewing."],
  ],
  compare_models: [
    ["selections", "Comparison models", "Select the models and their order. Each receives the same request and context."],
  ],
  agent_role: [
    ["role", "Agent role", "Select the instructions and responsibilities used by this stage."],
    ["model", "Stage model override", "Choose a model for this stage or inherit the runtime default."],
    ["requirements", "Stage requirements", "Set the required capabilities and, where available, configure planning or goal behavior."],
  ],
  required_checks: [
    ["names", "Required checks", "List the check names to run, one per line."],
  ],
  condition: [
    ["predicate", "Branch condition", "Test whether a JSON Pointer exists, whether it equals a value of the same type, or whether candidate checks passed."],
  ],
  command: [
    ["registrationId", "Local command registration", "Select a registered executable or script at its pinned revision."],
    ["arguments", "Arguments", "Provide one argument per line."],
    ["input", "Standard input", "Provide a JSON object as the command's input."],
    ["output", "Standard output", "Choose whether to capture stdout as text or parse it as JSON."],
  ],
  mcp_tool: [
    ["serverId", "Local MCP server and tool", "Choose a registered server and tool at the pinned server and contract revisions."],
    ["arguments", "Tool arguments", "Use fixed values or map fields from the Arguments input using JSON Pointers."],
    ["destinationPointer", "Destination argument pointer", "Identify where the selected destination is supplied in the tool arguments when the call uses one."],
  ],
  for_each: [
    ["maxItems", "Maximum items", "Set the maximum list length to process, from 1 to " + MAX_ITERATION_ITEMS + "."],
    ["readyInput", "Subflow inputs", "Choose the control input that starts the subflow and the JSON item input that receives each item."],
    ["resultOutput", "Subflow outputs", "Choose the completion control port and the data port whose value is collected."],
  ],
  repeat_until: [
    ["maxIterations", "Maximum iterations", "Set the limit, from 1 to " + MAX_ITERATION_ITEMS + " iterations."],
    ["completionOutput", "Subflow ports", "Choose the ready input, completion control output and data output to test."],
    ["until", "Stopping condition", "Test the returned evidence after an iteration to decide whether to stop."],
    ["feedback", "Pass a result to the next iteration", "Optionally connect an output to a feedback input of the same data type."],
  ],
};

export const nodeExamples: Partial<Record<string, Example>> = {
  validate_output: ["Check a backlog proposal", "Require a tasks array with stable keys and descriptions before the proposal reaches Materialize backlog."],
  collect_evidence: ["Gather independent reviews", "Connect architecture, correctness and test evidence so the quality gate can see each branch outcome."],
  quality_gate: ["Request a plan revision", "Check the findings from each review and send the revise outcome back to the planning loop."],
  materialize_backlog: ["Turn a specification into trackable work", "Connect the approved plan, SDD and BDD. Pass the resulting backlog to Run backlog."],
  run_backlog: ["Deliver tasks in dependency order", "Choose a published delivery subflow and a parallel task limit. Use the integrations it records as delivery evidence."],
  verify_test_phase: ["Verify the new regression test", "Run Red on the pinned base with the test-only patch. Pass the recorded failure forward to Green after implementation."],
  verify_integrated_checks: ["Test the integrated backlog", "Connect Run backlog's integrated output and select the build and test checks. Route verified to Result and failed or blocked to review."],
  search_public_repositories: ["Research implementation examples", "Connect a query from Build text and pass the resulting repository research to the planning agent."],
  open_or_update_pull_request: ["Publish a reviewed change", "Connect verified commit evidence and the risk assessment before handing the pull request to Wait for CI."],
  wait_for_ci: ["Wait for required checks", "Connect the recorded pull request and use the passed, failed, or stale port to decide what to do next."],
  merge_pull_request: ["Record the integrated change", "Connect the CI evidence and applicable approval, then use the recorded merge in delivery tracking."],
  agent: ["Summarize a release", "Feed a release-note prompt from Build text and use a read-only role to produce a summary for the result."],
  typed_start: ["Run for a release version", "Add a required text field named version. The operator supplies it at launch, and Build text can format it into a brief."],
  start: ["Plan from a saved note", "Choose a saved Markdown revision at launch, then connect document to Load context."],
  summarize_result: ["Finish with a run summary", "Connect a rendered summary and the recorded evidence to give the operator one result to review."],
  load_work_context: ["Give a reviewer task context", "Choose Task and repository Markdown and connect the task and its source revision."],
  load_context: ["Prepare a planning input", "Connect the saved document from Start and send context to Plan."],
  manage_work: ["Create a follow-up sub-task", "Choose Create Sub-Task, connect the parent Task and supply a title from Build text."],
  find_work: ["Locate evidence to review", "Filter Tasks by a distinctive title and choose Exactly one match to use its Task output."],
  update_progress: ["Check a completed item", "Connect a checklist target from Find work and choose Check item after validation succeeds."],
  set_fields: ["Prepare a report object", "Build a new object and set /status to a fixed value. Copy another field from the connected source."],
  filter_map_list: ["Collect ready items", "Keep entries where /status equals ready, then copy /title to /label for the next step."],
  build_text: ["Format a release note", "Connect an object containing version and render it with the template Release {{version}}."],
  build_list: ["Prepare validation targets", "Configure two target values, then connect items to For each to handle them individually."],
  request_input: ["Confirm a migration requirement", "Ask whether a migration is needed, then pass the Boolean answer to a Condition."],
  approval: ["Approve a review outcome", "Attach the review as Evidence, then connect approved and rejected to their respective paths."],
  compare_models: ["Compare two implementation plans", "Connect the same read-only planning request and Markdown context, then inspect the individual results."],
  plan: ["Plan changes from a spec", "Connect Markdown from Load context and configure the planning role. Send the plan output to Review plan."],
  review_plan: ["Check a migration plan", "Connect the plan and ask a read-only reviewer to identify missing verification steps."],
  link_work: ["Track approved preparation", "Connect a plan and review to create or link the Task the workflow will later implement."],
  approve_implementation: ["Release a reviewed change", "Connect the plan and task, then route the recorded decision into Prepare worktree."],
  prepare_worktree: ["Isolate approved work", "Combine the approved decision and Task, then connect worktree and release to Implement."],
  implement: ["Apply an approved fix", "Supply the plan, release and worktree. Send the captured candidate to verification."],
  review_candidate: ["Inspect a candidate patch", "Connect the captured candidate and use a read-only review role to record findings before acceptance."],
  read_git: ["Check a selected diff", "Choose the Git state at run launch and send the recorded inspection to Review diff."],
  review_diff_candidate: ["Review the current changes", "Connect a recorded Git inspection and configure a role to review the selected diff."],
  verify_review_diff: ["Run checks against a diff", "Connect its Git inspection and specify the required check names."],
  publish_diff_report: ["Share a checked review", "Connect a Git inspection, model review and check results to publish one report."],
  verify_candidate: ["Verify a captured candidate", "Enter the registered check names, connect the candidate and send checks to Accept candidate."],
  accept_candidate: ["Record an acceptance", "Supply a captured candidate, its review and its check results before Verify commit."],
  verify_commit: ["Confirm the accepted commit", "Connect the accepted candidate to record verified Git evidence for the next step."],
  verify_merge: ["Confirm integration", "Pass the verified commit to check whether it is integrated into the configured destination."],
  activate_maintenance: ["Record a maintenance activation", "Pass verified Git evidence from the accepted maintenance candidate into the activation step."],
  command: ["Run the repository test script", "Select a registered script revision, choose the expected output format and supply approved release and worktree inputs."],
  mcp_tool: ["Look up a tracked issue", "Choose an approved tool and map the issue ID from a connected artifact into its argument."],
  condition: ["Route an approved response", "Test whether /approved equals true, then continue through matched or unmatched."],
  parallel: ["Start independent checks", "Connect multiple control branches to branches so they can run separately."],
  join: ["Continue after two checks", "Feed the branch control outputs into branches, then use joined to continue."],
  subflow: ["Reuse a saved check", "Select a published subflow, then match its public inputs and outputs to the surrounding nodes."],
  repeat_until: ["Retry a review with a bound", "Choose the child review's outputs, stop when returned evidence passes, and cap the number of iterations."],
  iteration_limit: ["Handle a loop limit", "Route the loop's limit path to this step to stop with a failed outcome."],
  for_each: ["Validate each item", "Connect the Build list output, choose the child workflow's item and result ports, and consume the collected results."],
  result: ["Publish a delivery result", "Connect the Task and verified Git evidence, then record the final delivery."],
};

export const simulationKinds: Partial<Record<string, string>> = {
  Start: "typed_start",
  "Load context": "load_context",
  Result: "summarize_result",
  "Set fields": "set_fields",
  "Filter and map list": "filter_map_list",
  "Build text": "build_text",
  Condition: "condition",
  Parallel: "parallel",
  Join: "join",
  "For each": "for_each",
  "Repeat until": "repeat_until",
  Agent: "agent",
  "Compare models": "compare_models",
  Command: "command",
  "MCP tool": "mcp_tool",
  "Request input": "request_input",
  Approval: "approval",
  "Approve implementation": "approve_implementation",
  "Find work": "find_work",
  "Create or link work": "manage_work",
  "Update progress": "update_progress",
  "Prepare worktree": "prepare_worktree",
  "Read Git": "read_git",
  "Verify candidate": "verify_candidate",
  "Accept diff": "accept_candidate",
  Commit: "verify_commit",
  Merge: "verify_merge",
  Plan: "plan",
  Implement: "implement",
  Review: "review_candidate",
  Build: "command",
  Lint: "command",
  Test: "command",
  "Review and test": "verify_candidate",
};

export const simulationExamples: Partial<Record<string, Example>> = {
  Commit: ["Record an accepted patch", "Use the acceptance record and expected source ref to save the candidate commit before integration."],
  Merge: ["Integrate a verified commit", "Take the verified source commit and check the expected destination before recording integration."],
  Build: ["Compile a candidate", "Run the registered build command and record the build result alongside the candidate."],
  Lint: ["Check candidate style", "Run the registered lint command and record the findings for review."],
  Test: ["Run regression tests", "Run the registered test command and retain its result as verification evidence."],
  "Review and test": ["Prepare a candidate decision", "Combine review findings with check evidence before accepting the candidate."],
};

const controlPortNotes: Partial<Record<string, string>> = {
  ready: "Starts this step after an upstream step is ready.",
  start: "Signals the start of the workflow.",
  completed: "Continues after this step completes.",
  failed: "Routes a failed outcome.",
  matched: "Continues when the condition is true.",
  unmatched: "Continues when the condition is false.",
  approved: "Continues when the request is approved.",
  rejected: "Continues when the request is declined.",
  branches: "Carries control between branches.",
  joined: "Continues after the connected branches finish.",
};

export function nodePortNote(name: string, kind: "control" | "data" | "evidence") {
  if (kind === "evidence") return "Evidence or a data artifact from another step.";
  return kind === "control" ? controlPortNotes[name] : undefined;
}
