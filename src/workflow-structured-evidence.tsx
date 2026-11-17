import { CircleAlert, CircleCheck, CircleMinus, ShieldCheck } from "lucide-react";
import { useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import type { WorkspaceProps } from "@/workbench-contract";
import { workflowRunTarget, type ExecutionArtifact } from "@/workflow-execution-model";
import "./workflow-structured-evidence.css";

type EvidenceScope = Pick<WorkspaceProps, "openWorkspace"> & { projectId: string };
type EvidenceTone = "success" | "warning" | "danger" | "neutral";

function record(value: unknown): Record<string, unknown> | undefined {
  if (value && typeof value === "object" && !Array.isArray(value)) return value as Record<string, unknown>;
}

function nonempty(value: unknown): value is string {
  return typeof value === "string" && Boolean(value.trim());
}

function strings(value: unknown) {
  return Array.isArray(value) ? value.filter(nonempty) : [];
}

function EvidenceStatus({ tone, children }: { tone: EvidenceTone; children: ReactNode }) {
  const Icon = tone === "success" ? CircleCheck : tone === "neutral" ? CircleMinus : CircleAlert;
  return <span className="workflow-structured-status" data-tone={tone}><Icon size={14} aria-hidden /> {children}</span>;
}

type CheckOutcome = { label: string; tone: EvidenceTone };
type CheckRow = { key: string; name: string; outcome?: CheckOutcome; detail?: string };

function commandOutcome(status: unknown): CheckOutcome | undefined {
  switch (status) {
    case "passed": return { label: "Passed", tone: "success" };
    case "failed": return { label: "Failed", tone: "danger" };
    case "could_not_run": return { label: "Could not run", tone: "warning" };
    case "interrupted": return { label: "Interrupted", tone: "warning" };
  }
}

function VerdictStatus({ verdict }: { verdict: "verified" | "failed" | "blocked" }) {
  return verdict === "verified" ? <EvidenceStatus tone="success">Verified</EvidenceStatus>
    : verdict === "failed" ? <EvidenceStatus tone="danger">Failed</EvidenceStatus>
      : <EvidenceStatus tone="warning">Blocked</EvidenceStatus>;
}

function CheckRows({ rows, label }: { rows: CheckRow[]; label: string }) {
  if (!rows.length) return null;
  return <ul className="workflow-structured-rows" aria-label={label}>{rows.map(({ key, name, outcome, detail }) =>
    <li key={key}>
      <div><strong>{name}</strong>{detail && <small>{detail}</small>}</div>
      {outcome && <EvidenceStatus tone={outcome.tone}>{outcome.label}</EvidenceStatus>}
    </li>)}</ul>;
}

function Facts({ items }: { items: [string, unknown][] }) {
  const available = items.flatMap(([label, value]) => nonempty(value) ? [{ label, value }] : []);
  return available.length ? <dl className="workflow-structured-facts">
    {available.map(({ label, value }) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}
  </dl> : null;
}

function EvidenceList({ title, items }: { title: string; items: string[] }) {
  return items.length ? <div className="workflow-structured-list">
    <strong>{title}</strong>
    <ul>{items.map((label, index) => <li key={`${label}:${index}`}>{label}</li>)}</ul>
  </div> : null;
}

function QualityGateEvidence({ report }: { report: Record<string, unknown> }) {
  const { decision } = report;
  if (decision !== "passed" && decision !== "revise" && decision !== "blocked") return null;
  return <section className="workflow-structured-evidence" aria-label="Quality gate decision">
    <div className="workflow-structured-heading"><span><ShieldCheck size={15} aria-hidden /> Quality gate</span>
      {decision === "passed" ? <EvidenceStatus tone="success">Passed</EvidenceStatus>
        : decision === "revise" ? <EvidenceStatus tone="warning">Revision needed</EvidenceStatus>
          : <EvidenceStatus tone="danger">Blocked</EvidenceStatus>}
    </div>
    <EvidenceList title="Blocked evidence sources" items={strings(report.blockers)} />
    <EvidenceList title="Rules to revisit" items={strings(report.findings)} />
  </section>;
}

function CollectedEvidence({ evidence }: { evidence: unknown }) {
  if (!Array.isArray(evidence)) return null;
  const sources = evidence.flatMap((entry: unknown) => {
    const item = record(entry);
    const source = record(item?.source);
    if (!source || !nonempty(source.node) || !nonempty(source.port)) return [];
    const state = item?.state;
    if (state !== "completed" && state !== "failed" && state !== "skipped") return [];
    return [{ node: source.node, port: source.port, state, artifactId: item?.artifactId }];
  });
  if (evidence.length && !sources.length) return null;
  return <section className="workflow-structured-evidence" aria-label="Collected evidence">
    <div className="workflow-structured-heading"><span>Collected evidence</span></div>
    {!evidence.length && <p>No evidence sources recorded.</p>}
    <ul className="workflow-structured-rows">{sources.map(({ node, port, state, artifactId }, index) =>
      <li key={`${node}:${port}:${index}`}>
        <div><strong><code>{node}.{port}</code></strong>
          {state === "completed" && nonempty(artifactId) && <small>Artifact <code>{artifactId}</code></small>}
        </div>
        {state === "completed" ? <EvidenceStatus tone="success">Collected</EvidenceStatus>
          : state === "failed" ? <EvidenceStatus tone="danger">Source failed</EvidenceStatus>
            : <EvidenceStatus tone="neutral">Skipped</EvidenceStatus>}
      </li>)}</ul>
  </section>;
}

const testPhaseNames = { baseline: "Baseline", red: "Red", green: "Green" };
const testSuiteLabels: Record<string, string> = {
  completed: "Completed", environment_error: "Environment error", build_error: "Build error", tool_error: "Tool error",
};

function TestPhaseEvidence({ report }: { report: Record<string, unknown> }) {
  const { phase, check } = report;
  const result = record(report.result);
  const verdict = result?.verdict;
  if ((phase !== "baseline" && phase !== "red" && phase !== "green") || !nonempty(check) ||
    !result || (verdict !== "verified" && verdict !== "failed" && verdict !== "blocked")) return null;
  const details = record(result.report);
  const specification = record(report.specification);
  const expectedFailures = Array.isArray(specification?.tests) ? specification.tests.flatMap((entry: unknown) => {
    const test = record(entry);
    return test && nonempty(test.id) && nonempty(test.reason) ? [{ id: test.id, reason: test.reason }] : [];
  }) : [];
  const tests = Array.isArray(details?.tests) ? details.tests.flatMap((entry: unknown) => {
    const test = record(entry);
    if (!test || !nonempty(test.id)) return [];
    const { id, status, reason } = test;
    return status === "passed" || status === "failed" || status === "skipped" ? [{ id, status, reason }] : [];
  }) : [];
  return <section className="workflow-structured-evidence" aria-label="Test phase evidence">
    <div className="workflow-structured-heading"><span>{testPhaseNames[phase]} test phase</span>
      {verdict === "verified" && phase === "red" ? <EvidenceStatus tone="success">Expected failure reproduced</EvidenceStatus>
        : <VerdictStatus verdict={verdict} />}
    </div>
    {nonempty(result.reason) && <p>{result.reason}</p>}
    {nonempty(details?.reason) && details.reason !== result.reason && <p>{details.reason}</p>}
    <Facts items={[["Registered check", check], ["Base commit", report.baseCommit], ["Tested tree", result.tree],
      ["Test patch revision", result.testPatchRevision],
      ["Test suite", nonempty(details?.status) && Object.hasOwn(testSuiteLabels, details.status) ? testSuiteLabels[details.status] : undefined],
      ["Exit code", typeof result.exitCode === "number" && Number.isInteger(result.exitCode) ? String(result.exitCode) : undefined]]} />
    {!!expectedFailures.length && <details>
      <summary>Expected failures ({expectedFailures.length})</summary>
      <ul className="workflow-structured-rows">{expectedFailures.map(({ id, reason }) =>
        <li key={id}><div><strong>{id}</strong><small>{reason}</small></div></li>)}</ul>
    </details>}
    {!!tests.length && <details>
      <summary>Recorded test results ({tests.length})</summary>
      <ul className="workflow-structured-rows">{tests.map(({ id, status, reason }, index) =>
        <li key={`${id}:${index}`}>
          <div><strong>{id}</strong>{nonempty(reason) && <small>{reason}</small>}</div>
          <EvidenceStatus tone={status === "passed" ? "success" : status === "failed" ? "danger" : "neutral"}>
            {status === "passed" ? "Passed" : status === "failed" ? "Failed" : "Skipped"}
          </EvidenceStatus>
        </li>)}</ul>
    </details>}
  </section>;
}

function IntegratedRevisionEvidence({ report }: { report: Record<string, unknown> }) {
  if (!nonempty(report.destinationBranch) || !nonempty(report.sha)) return null;
  const integrations = strings(report.integrationShas);
  return <section className="workflow-structured-evidence" aria-label="Integrated revision">
    <div className="workflow-structured-heading"><span>Integrated revision</span></div>
    <Facts items={[["Destination branch", report.destinationBranch], ["Pinned commit", report.sha]]} />
    {!!integrations.length && <details>
      <summary>Included task merges ({integrations.length})</summary>
      <ul className="workflow-structured-rows">{integrations.map((sha, index) =>
        <li key={`${sha}:${index}`}><code>{sha}</code></li>)}</ul>
    </details>}
  </section>;
}

function IntegratedChecksEvidence({ report }: { report: Record<string, unknown> }) {
  const revision = record(report.revision);
  const verdict = report.verdict;
  if (!revision || !nonempty(revision.sha) ||
    (verdict !== "verified" && verdict !== "failed" && verdict !== "blocked")) return null;
  const checks: CheckRow[] = Array.isArray(report.checks) ? report.checks.flatMap((entry: unknown) => {
    const check = record(entry);
    if (!check || !nonempty(check.name)) return [];
    const outcome = record(check.outcome);
    return [{ key: check.name, name: check.name, outcome: commandOutcome(outcome?.status),
      detail: nonempty(outcome?.reason) ? outcome.reason : undefined }];
  }) : [];
  return <section className="workflow-structured-evidence" aria-label="Integrated checks">
    <div className="workflow-structured-heading"><span>Integrated checks</span><VerdictStatus verdict={verdict} /></div>
    {nonempty(report.reason) && <p>{report.reason}</p>}
    <Facts items={[["Destination branch", revision.destinationBranch], ["Pinned commit", revision.sha], ["Tested tree", report.tree]]} />
    <CheckRows rows={checks} label="Check results" />
  </section>;
}

function CandidateChecksEvidence({ projectId, taskId, verificationId }: {
  projectId: string; taskId: string; verificationId: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const result = useQuery({
    queryKey: ["delivery-verification", projectId, taskId, verificationId],
    queryFn: () => invoke<unknown>("inspect_candidate_verification", { projectId, taskId, verificationId }),
    enabled: expanded,
    retry: false,
  });
  const report = record(result.data);
  const checks: CheckRow[] = report?.id === verificationId && Array.isArray(report.gates) ? report.gates.flatMap((entry: unknown) => {
    const check = record(entry);
    const gate = record(check?.gate);
    if (!gate || !nonempty(gate.name)) return [];
    const outcome = record(check?.outcome);
    const state = check?.deferred === true ? { label: "Deferred", tone: "neutral" as const }
      : outcome ? commandOutcome(outcome.status) : { label: "Pending", tone: "neutral" as const };
    return [{ key: gate.name, name: gate.name, outcome: state,
      detail: nonempty(outcome?.reason) ? outcome.reason : undefined }];
  }) : [];
  return <details className="workflow-structured-evidence" onToggle={(event) => setExpanded(event.currentTarget.open)}>
    <summary>Candidate check results</summary>
    {expanded && result.isPending && <p role="status">Loading recorded checks…</p>}
    {expanded && result.isError && <p role="alert">{String(result.error)}</p>}
    {expanded && result.isSuccess && (report?.id !== verificationId
      ? <p>Check evidence is unavailable in this project.</p>
      : !checks.length ? <p>No check results were recorded.</p> : <CheckRows rows={checks} label="Candidate checks" />)}
  </details>;
}

function GitHubPullEvidence({ report }: { report: Record<string, unknown> }) {
  const pull = record(report.pull);
  if (!pull || typeof pull.number !== "number" || !Number.isSafeInteger(pull.number) || pull.number <= 0) return null;
  return <section className="workflow-structured-evidence" aria-label="GitHub pull request">
    <div className="workflow-structured-heading"><span>GitHub pull request #{pull.number}</span>
      {pull.merged === true ? <EvidenceStatus tone="success">Merged</EvidenceStatus>
        : pull.open === true ? <EvidenceStatus tone="neutral">{pull.draft === true ? "Draft" : "Open"}</EvidenceStatus>
          : pull.open === false && pull.merged === false ? <EvidenceStatus tone="neutral">Closed</EvidenceStatus> : null}
    </div>
    {report.requiresHuman === true && <p>Delivery policy requires human approval.</p>}
    <Facts items={[["Pull request URL", pull.url], ["Head branch", pull.headBranch], ["Head commit", pull.headSha],
      ["Base branch", pull.baseBranch], ["Base commit", pull.baseSha], ["Merge commit", pull.mergeCommitSha]]} />
  </section>;
}

function ciOutcome(state: unknown): CheckOutcome | undefined {
  switch (state) {
    case "passed": return { label: "Passed", tone: "success" };
    case "failed": return { label: "Failed", tone: "danger" };
    case "pending": return { label: "Pending", tone: "neutral" };
    case "unavailable": return { label: "Unavailable", tone: "warning" };
  }
}

function GitHubCiEvidence({ report }: { report: Record<string, unknown> }) {
  const snapshot = record(report.snapshot);
  const state = ciOutcome(snapshot?.state);
  if (!snapshot || !state) return null;
  const pull = record(record(report.pull)?.pull);
  const checks: CheckRow[] = Array.isArray(snapshot.checks) ? snapshot.checks.flatMap((entry: unknown, index: number) => {
    const check = record(entry);
    if (!check || !nonempty(check.context)) return [];
    return [{ key: `${check.context}:${index}`, name: check.context, outcome: ciOutcome(check.state) }];
  }) : [];
  const required = Array.isArray(snapshot.requiredChecks) ? snapshot.requiredChecks.flatMap((entry: unknown) => {
    const context = record(entry)?.context;
    return nonempty(context) ? [context] : [];
  }) : [];
  return <section className="workflow-structured-evidence" aria-label="GitHub CI evidence">
    <div className="workflow-structured-heading"><span>GitHub CI</span>
      <EvidenceStatus tone={state.tone}>{state.label}</EvidenceStatus>
    </div>
    {nonempty(snapshot.reason) && <p>{snapshot.reason}</p>}
    <Facts items={[["Tested commit", snapshot.testedSha], ["Tested tree", snapshot.testedTree],
      ["Pull request head", snapshot.headSha], ["Pull request base", snapshot.baseSha], ["Pull request URL", pull?.url]]} />
    <CheckRows rows={checks} label="GitHub check results" />
    <EvidenceList title="Required checks" items={required} />
  </section>;
}

function GitHubResearchEvidence({ report }: { report: Record<string, unknown> }) {
  if (!nonempty(report.query) || !Array.isArray(report.repositories)) return null;
  const repositories = report.repositories.flatMap((entry: unknown) => {
    const item = record(entry);
    const slug = record(record(item?.repository)?.slug);
    if (!item || !slug || !nonempty(slug.owner) || !nonempty(slug.name)) return [];
    return [{ name: `${slug.owner}/${slug.name}`, url: item.url, revision: item.revision, license: item.license, description: item.description }];
  });
  return <section className="workflow-structured-evidence" aria-label="GitHub search evidence">
    <div className="workflow-structured-heading"><span>GitHub search</span></div>
    <Facts items={[["Query", report.query]]} />
    {!report.repositories.length && <p>No repositories were recorded for this query.</p>}
    {!!repositories.length && <details><summary>Repositories ({repositories.length})</summary>
      <ul className="workflow-structured-rows">{repositories.map(({ name, url, revision, license, description }) => <li key={name}>
        <div><strong>{name}</strong>{nonempty(description) && <p>{description}</p>}
          <Facts items={[["Revision", revision], ["License", license], ["Repository URL", url]]} />
        </div>
      </li>)}</ul>
    </details>}
  </section>;
}

function BacklogEvidence({ report, projectId, openWorkspace }: EvidenceScope & { report: Record<string, unknown> }) {
  const { epicId } = report;
  if (!nonempty(epicId) || !Array.isArray(report.tasks)) return null;
  const tasks = report.tasks.flatMap((entry: unknown) => {
    const item = record(entry);
    const task = record(item?.task);
    const spec = record(item?.specification);
    return task && nonempty(task.taskId) && spec && nonempty(spec.key) && nonempty(spec.title)
      ? [{ id: task.taskId, project: task.projectId, key: spec.key, title: spec.title, dependsOn: strings(spec.dependsOn) }]
      : [];
  });
  return <section className="workflow-structured-evidence" aria-label="Materialized backlog">
    <div className="workflow-structured-heading"><span>Materialized backlog</span>
      <Button type="button" className="text-xs" variant="outline" onClick={() => openWorkspace("tasks", epicId)}>Open Epic</Button>
    </div>
    {!!tasks.length && <details>
      <summary>Recorded tasks ({tasks.length})</summary>
      <ul className="workflow-structured-rows">{tasks.map((task) =>
        <li key={task.id}>
          <div><strong>{task.title}</strong><small>{task.key}{task.dependsOn.length > 0 && <> · Depends on {task.dependsOn.join(", ")}</>}</small></div>
          {task.project === projectId && <Button type="button" className="text-xs" variant="ghost"
            aria-label={`Open task ${task.title}`} onClick={() => openWorkspace("tasks", task.id)}>Open task</Button>}
        </li>)}</ul>
    </details>}
  </section>;
}

function BacklogPlanEvidence({ report }: { report: Record<string, unknown> }) {
  const spec = record(record(report.task)?.specification);
  if (!spec || !nonempty(spec.title) || !nonempty(report.parentRunId)) return null;
  return <section className="workflow-structured-evidence" aria-label="Backlog delivery plan">
    <div className="workflow-structured-heading"><span>Backlog delivery plan</span></div>
    <p><strong>{spec.title}</strong></p>
    <Facts items={[["Task key", spec.key], ["Destination branch", report.destinationBranch]]} />
    <EvidenceList title="Acceptance criteria" items={strings(spec.acceptanceCriteria)} />
  </section>;
}

function BacklogDeliveryEvidence({ report, projectId, openWorkspace }: EvidenceScope & { report: Record<string, unknown> }) {
  if (!Array.isArray(report.tasks)) return null;
  const tasks = report.tasks.flatMap((entry: unknown) => {
    const item = record(entry);
    const task = record(item?.task);
    if (!item || !nonempty(item.key) || !nonempty(item.workflowRunId) || !nonempty(item.integrationSha)) return [];
    return [{ key: item.key, runId: item.workflowRunId, sha: item.integrationSha, branch: item.destinationBranch, project: task?.projectId }];
  });
  if (!tasks.length) return null;
  return <section className="workflow-structured-evidence" aria-label="Verified backlog integrations">
    <div className="workflow-structured-heading"><span>Verified backlog integrations</span></div>
    <ul className="workflow-structured-rows">{tasks.map(({ key, runId, sha, branch, project }) =>
      <li key={key}><div><strong>{key}</strong><small>{nonempty(branch) && <>{branch} · </>}<code>{sha}</code></small></div>
        {project === projectId && <Button type="button" className="text-xs" variant="ghost" aria-label={`Open workflow for ${key}`}
          onClick={() => openWorkspace("pipelines", workflowRunTarget(runId))}>Open workflow</Button>}
      </li>)}</ul>
  </section>;
}

export function WorkflowStructuredEvidence({ artifact, producer, ...scope }: EvidenceScope & { artifact: ExecutionArtifact; producer: string }) {
  const { kind, evidence } = artifact.value;
  if (kind === "evidence_set") return <CollectedEvidence evidence={evidence} />;
  const report = record(evidence);
  if (!report) return null;
  if (kind === "test_phase_evidence") return <TestPhaseEvidence report={report} />;
  if (kind === "integrated_revision") return <IntegratedRevisionEvidence report={report} />;
  if (kind === "integrated_checks_evidence") return <IntegratedChecksEvidence report={report} />;
  if (kind === "github_pull_request") return <GitHubPullEvidence report={report} />;
  if (kind === "github_ci") return <GitHubCiEvidence report={report} />;
  if (kind === "github_research") return <GitHubResearchEvidence report={report} />;
  if (kind === "backlog") return <BacklogEvidence report={report} {...scope} />;
  if (kind === "backlog_plan") return <BacklogPlanEvidence report={report} />;
  if (kind === "checks" && producer === "verify_candidate" && artifact.port === "checks") {
    const task = record(report.task);
    if (task?.projectId === scope.projectId && nonempty(task.taskId) && nonempty(report.id)) {
      return <CandidateChecksEvidence key={report.id} projectId={scope.projectId} taskId={task.taskId} verificationId={report.id} />;
    }
  }
  if (kind === "json_object" && producer === "quality_gate" && artifact.port === "decision") return <QualityGateEvidence report={report} />;
  if (kind === "json_object" && producer === "run_backlog" && artifact.port === "deliveries") return <BacklogDeliveryEvidence report={report} {...scope} />;
  return null;
}
