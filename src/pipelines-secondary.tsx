import { useEffect, useState } from "react";
import { ArrowRight, Check, CircleAlert, Columns2, GitBranch, ListChecks, Plus, ShieldCheck } from "lucide-react";
import { PipelineComparisonCard, PipelineComparisonChoice, PipelineComparisonSetup } from "@/pipeline-comparison-layout";
import { PipelineMaintenanceLayout, PipelinePluginItem, PipelineSectionHeading, PipelineTemplateList } from "@/pipeline-layout";
import { Button } from "@/components/ui/button";
import { EffortSelect } from "@/components/effort-select";
import { ModelPicker } from "@/components/model-picker";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { WorkspaceProps } from "@/workbench-contract";
import { exampleModelEfforts } from "@/model-effort-examples";
import { getSampleModel, sampleModels } from "./pipelines-model";
import type { Role, WorkflowRevision } from "./pipelines-model";

export function PendingPipeline({ onSimulate, label }: { onSimulate: () => void; label: string }) {
  return (
    <div className="pipeline-empty">
      <div className="pipeline-empty-icon"><GitBranch size={22} /></div>
      <h3>{label}</h3>
      <p>Integration pending. Open the pipeline scenario to try this workflow.</p>
      <Button variant="outline" onClick={onSimulate}>Simulate pipelines <ArrowRight size={15} /></Button>
    </div>
  );
}

export function RoleEditor({ roles, onChange }: { roles: Role[]; onChange: (roles: Role[]) => void }) {
  const [selected, setSelected] = useState(roles[0]?.id ?? "");
  const [newRoleName, setNewRoleName] = useState("");
  const role = roles.find((item) => item.id === selected) ?? roles[0];
  function update(patch: Partial<Role>) {
    if (!role) return;
    onChange(roles.map((item) => item.id === role.id ? { ...item, ...patch } : item));
  }
  function addRole(event: React.FormEvent) {
    event.preventDefault();
    const title = newRoleName.trim();
    if (!title || roles.some((item) => item.title.toLowerCase() === title.toLowerCase())) return;
    const id = crypto.randomUUID();
    onChange([...roles, { id, title, model: null, effort: "", instructions: "", tools: "Repository read", completion: "", review: "Independent", requirements: [] }]);
    setNewRoleName("");
    setSelected(id);
  }
  const requirements = ["Interactive questions", "Repository read", "Image generation", "Audio generation", "Windows host control"];
  const sources = ["AGENTS.md (workspace)", "Codex · release-review (project skill)", "Claude Code · test-runner (approved plugin)"];
  return (
    <div className="pipeline-two-columns">
      <section className="pipeline-box">
        <h3>Roles</h3>
        <ul className="pipeline-select-list">
          {roles.map((item) => <li key={item.id}><button type="button" aria-pressed={role?.id === item.id} onClick={() => setSelected(item.id)}><ShieldCheck size={15} />{item.title || "Untitled role"}</button></li>)}
        </ul>
        <form className="pipeline-stack pipeline-form" onSubmit={addRole}>
          <label>New role name<input value={newRoleName} onChange={(event) => setNewRoleName(event.currentTarget.value)} /></label>
          <Button type="submit" variant="outline" disabled={!newRoleName.trim() || roles.some((item) => item.title.toLowerCase() === newRoleName.trim().toLowerCase())}><Plus size={15} /> Add role</Button>
        </form>
      </section>
      {role && (
        <section className="pipeline-box pipeline-stack pipeline-form">
          <h3>{role.title || "Untitled role"}</h3>
          <label>Name<input value={role.title} onChange={(event) => update({ title: event.currentTarget.value })} /></label>
          <ModelPicker label="Default model" options={sampleModels} selection={{ mode: "single", selected: role.model, onChange: (model) => {
            const effort = exampleModelEfforts(model)?.efforts.includes(role.effort) ? role.effort : "";
            update({ model, effort });
          } }} />
          <label>Effort<EffortSelect capabilities={exampleModelEfforts(role.model)} value={role.effort} onChange={(effort) => update({ effort })} /></label>
          <label>Instructions<textarea rows={3} value={role.instructions} onChange={(event) => update({ instructions: event.currentTarget.value })} /></label>
          <label>Tool scope<select value={role.tools} onChange={(event) => update({ tools: event.currentTarget.value })}><option>Repository read</option><option>Approved worktree write</option><option>No tools</option></select></label>
          <label>Completion contract<textarea rows={2} value={role.completion} onChange={(event) => update({ completion: event.currentTarget.value })} /></label>
          <label>Review policy<select value={role.review} onChange={(event) => update({ review: event.currentTarget.value })}><option>Independent</option><option>Cross-review</option><option>Human approval</option></select></label>
          <fieldset className="pipeline-checks">
            <legend>Required capabilities and customizations</legend>
            {[...requirements, ...sources].map((requirement) => (
              <label key={requirement}><input type="checkbox" checked={role.requirements.includes(requirement)} onChange={(event) => update({ requirements: event.currentTarget.checked ? [...role.requirements, requirement] : role.requirements.filter((item) => item !== requirement) })} />{requirement}</label>
            ))}
          </fieldset>
          <details className="pipeline-evidence">
            <summary>Effective inventory</summary>
            <p>Worktree release/notes · AGENTS.md and release-review at the pinned context revision.</p>
            {role.requirements.includes("Windows host control") && <p role="status">Windows host control is unverified for this example.</p>}
          </details>
        </section>
      )}
    </div>
  );
}

export function TemplateHistory({ revisions, onLoad }: { revisions: WorkflowRevision[]; onLoad: (revision: WorkflowRevision) => void }) {
  return (
    <section className="pipeline-box">
      <h3>Saved templates</h3>
      <PipelineTemplateList entries={[...revisions].reverse().map((revision) => ({
        id: String(revision.revision), name: revision.label, detail: `Revision ${revision.revision} · ${revision.nodes.length} stages`,
      }))} onOpen={(id) => {
        const revision = revisions.find((item) => String(item.revision) === id);
        if (revision) onLoad(revision);
      }} />
    </section>
  );
}

type CandidateStatus = "queued" | "running" | "completed" | "failed";
interface Candidate {
  selection: ModelSelection;
  status: CandidateStatus;
}
interface ComparisonRun {
  id: number;
  prompt: string;
  context: string;
  candidates: Candidate[];
}

const initialSelections = ["gpt-6.1-sol", "gpt-6-luna", "claude-sonnet-5-5"].flatMap((id) => {
  const model = getSampleModel(id);
  return model ? [model.selection] : [];
});

const sampleContexts = ["docs/release.md @ 41ac02e", "docs/testing.md @ 48ecf27"];

const responseExamples = [
  "Add a release checklist to the pinned note. Require the run to record the candidate revision, review decision, and verification output before accepting the patch.",
  "Keep release documentation and verification separate. Add an explicit check for an approved plan revision and link the resulting report back to the task.",
  "Prioritize the workflow handoff. Have the reviewer confirm the exact commit and ensure a rerun cannot replace the accepted evidence.",
];

function nextComparison(run: ComparisonRun): ComparisonRun {
  const running = run.candidates.findIndex((candidate) => candidate.status === "running");
  const queued = run.candidates.findIndex((candidate) => candidate.status === "queued");
  if (running < 0 && queued < 0) return run;
  return {
    ...run,
    candidates: run.candidates.map((candidate, index) => {
      if (running === index) return { ...candidate, status: index === 1 ? "failed" : "completed" };
      if (queued === index) return { ...candidate, status: "running" };
      return candidate;
    }),
  };
}

export function ComparisonView({ simulated, onSimulate, onUse }: { simulated: boolean; onSimulate: () => void; onUse: (results: string[]) => void }) {
  const [selection, setSelection] = useState<ModelSelection[]>(simulated ? initialSelections : []);
  const [prompt, setPrompt] = useState(simulated ? "Review the pinned release plan and propose verification criteria." : "");
  const [context, setContext] = useState(simulated ? sampleContexts[0] ?? "" : "");
  const [run, setRun] = useState<ComparisonRun>();
  const [included, setIncluded] = useState<string[]>([]);
  const [synthesis, setSynthesis] = useState("");
  const active = run?.candidates.some((candidate) => candidate.status === "queued" || candidate.status === "running") ?? false;
  useEffect(() => {
    if (!run || !active) return;
    const timer = window.setTimeout(() => setRun((current) => current ? nextComparison(current) : current), 650);
    return () => window.clearTimeout(timer);
  }, [run, active]);

  function start() {
    if (!simulated || !prompt.trim() || !context || selection.length < 2 || active) return;
    setSynthesis("");
    setIncluded([]);
    setRun({ id: (run?.id ?? 0) + 1, prompt: prompt.trim(), context, candidates: selection.map((item) => ({ selection: item, status: "queued" })) });
  }
  const selectedIds = new Set(selection.map((item) => item.modelId));
  const orderedModels = [...selection.flatMap((item) => {
    const model = getSampleModel(item.modelId);
    return model ? [model] : [];
  }), ...sampleModels.filter((item) => !selectedIds.has(item.selection.modelId))];
  const successful = run?.candidates.filter((candidate) => candidate.status === "completed").length ?? 0;
  const failed = run?.candidates.filter((candidate) => candidate.status === "failed").length ?? 0;
  const title = active ? "Comparison in progress" : successful && failed ? "Partial result" : successful ? "Comparison complete" : "Comparison failed";
  return (
    <div className="pipeline-stack">
      <PipelineSectionHeading title="Compare models" description="Use one pinned request and context for independently reported responses."
        icon={<Columns2 size={20} aria-hidden />} />
      {!simulated && <PendingPipeline label="No model comparisons" onSimulate={onSimulate} />}
      <PipelineComparisonSetup onSubmit={(event) => { event.preventDefault(); start(); }} request={<>
          <label>Prompt<textarea rows={4} value={prompt} onChange={(event) => setPrompt(event.currentTarget.value)} disabled={!simulated || active} /></label>
          <label>Pinned context<select value={context} onChange={(event) => setContext(event.currentTarget.value)} disabled={!simulated || active}>
            {!simulated && <option value="">Select a source revision</option>}
            {simulated && sampleContexts.map((source) => <option key={source}>{source}</option>)}
          </select></label>
          {simulated && <p className="pipeline-note"><ShieldCheck size={15} /> Verified read-only example · clean comparison checkout · one slot</p>}
          <Button type="submit" disabled={!simulated || !prompt.trim() || !context || selection.length < 2 || active}>Start comparison</Button>
        </>} candidates={<>
          <fieldset disabled={!simulated || active}>
            <ModelPicker label="Select models" options={simulated ? orderedModels : []} selection={{ mode: "multiple", selected: selection, onChange: setSelection }} />
          </fieldset>
      </>} />
      {run && (
        <>
          <div className="pipeline-section-heading" aria-live="polite"><div><h2>{title}</h2><p>{successful} completed · {failed} failed · {run.candidates.length} candidates</p></div></div>
          <ol className="pipeline-results" aria-label="Comparison results">
            {run.candidates.map(({ selection: candidate, status }, index) => {
              const model = getSampleModel(candidate.modelId);
              const response = responseExamples[index % responseExamples.length];
              return (
                <PipelineComparisonCard key={candidate.modelId} name={model?.name ?? candidate.modelId} state={status} rating={model?.rating ?? null}>
                  <p className="pipeline-meta">{candidate.provider} · {status === "completed" ? "1.2 s · 356 input / 92 output tokens" : status === "failed" ? "0.8 s · usage unavailable" : "Usage pending"}</p>
                  {status === "completed" && <><p>{response}</p><details className="pipeline-evidence"><summary>Reported reasoning</summary><p>Checked the pinned release criteria and the proposed verification boundary.</p></details></>}
                  {status === "failed" && <p className="pipeline-warning"><CircleAlert size={15} /> Example provider timed out.</p>}
                  <details className="pipeline-evidence"><summary>Context and dispatch</summary><p>{run.prompt}</p><p className="pipeline-meta">{run.context} · read-only tools · {candidate.provider} reported its own instruction inventory.</p></details>
                  {status === "completed" && <PipelineComparisonChoice checked={included.includes(candidate.modelId)}
                    onChange={(checked) => setIncluded(checked ? [...included, candidate.modelId] : included.filter((id) => id !== candidate.modelId))} />}
                </PipelineComparisonCard>
              );
            })}
          </ol>
          {!active && successful > 0 && <section className="pipeline-box pipeline-stack">
            <div className="pipeline-card-heading"><h3>Synthesis</h3><ListChecks size={16} /></div>
            <p className="pipeline-meta">Create a separate result from selected candidate runs.</p>
            <div className="pipeline-actions">
              <Button variant="outline" disabled={!included.length} onClick={() => setSynthesis("Check the release plan against the pinned candidate, record verification evidence, and require the exact plan revision before applying repository changes.")}>Generate synthesis</Button>
              <Button variant="outline" disabled={!included.length} onClick={() => onUse(run.candidates.flatMap(({ selection: candidate }, index) => included.includes(candidate.modelId) ? ["comparison " + run.id + "/" + (index + 1) + " (" + candidate.modelId + ")"] : []))}>Use in pipeline</Button>
            </div>
            {synthesis && <p>{synthesis}</p>}
          </section>}
        </>
      )}
    </div>
  );
}

interface Plugin {
  id: string;
  name: string;
  scope: string;
  state: "available" | "enabled" | "disabled";
}

const initialPlugins: Plugin[] = [
  { id: "cross-review", name: "Codex · Cross-review", scope: "Workspace · release/notes", state: "enabled" },
  { id: "test-runner", name: "Claude Code · Test runner", scope: "Project", state: "available" },
];

export function PipelineMaintenance({ simulation, startSimulation, openWorkspace, openChanges }: Pick<WorkspaceProps, "simulation" | "startSimulation" | "openWorkspace" | "openChanges">) {
  const [proposal, setProposal] = useState<"pending" | "accepted" | "rejected">("pending");
  const [plugins, setPlugins] = useState(initialPlugins);
  const [pluginHistory, setPluginHistory] = useState<string[]>([]);
  if (!simulation) return <PendingPipeline label="No maintenance proposals" onSimulate={() => startSimulation("pipelines")} />;
  return <PipelineMaintenanceLayout proposals={<>
    <h3>Setting proposal</h3>
    <p className="pipeline-meta">Run release-maintenance · Project · Expected revision 12</p>
    <dl className="pipeline-data"><dt>Setting</dt><dd>Default review policy</dd><dt>From</dt><dd>Single review</dd><dt>To</dt><dd>Independent cross-review</dd><dt>Decision</dt><dd>{proposal === "pending" ? "Human confirmation required" : proposal === "accepted" ? "Accepted · revision 13" : "Declined"}</dd></dl>
    <p className="pipeline-meta">Automatic changes are limited to fields approved for this project. Running workflows retain their pinned revisions.</p>
    {proposal === "pending" ? <div className="pipeline-actions"><Button onClick={() => setProposal("accepted")}>Accept proposal</Button><Button variant="outline" onClick={() => setProposal("rejected")}>Decline</Button></div> : <p role="status" className="pipeline-note"><Check size={15} /> Decision recorded for this scenario.</p>}
  </>} releases={<>
    <h3>Candidate package</h3>
    <dl className="pipeline-data"><dt>Running</dt><dd>0.1.0 · 41ac02e</dd><dt>Candidate</dt><dd>0.2.0 · 48ecf27</dd><dt>Digest</dt><dd className="font-mono">sha256:a48ecf27…5f99be3</dd><dt>Evidence</dt><dd>Build, checks and approved change</dd></dl>
    <div className="pipeline-actions"><Button variant="outline" onClick={() => openWorkspace("tasks", "release")}>Open task</Button><Button variant="outline" onClick={() => openChanges("saved")}>Open diff evidence</Button><Button variant="outline" onClick={() => startSimulation("windows")}>Open update simulation</Button></div>
  </>} inventory={<>
    {plugins.map((plugin) => {
      const action = plugin.state === "available" ? "Install" : plugin.state === "enabled" ? "Disable" : "Enable";
      const next = plugin.state === "enabled" ? "disabled" : "enabled";
      return <PipelinePluginItem key={plugin.id} name={plugin.name} detail={plugin.scope + " · " + plugin.state}
        action={<Button variant="outline" onClick={() => {
          setPlugins(plugins.map((item) => item.id === plugin.id ? { ...item, state: next } : item));
          setPluginHistory([...pluginHistory, plugin.name + " · " + plugin.state + " → " + next]);
        }}>{action}</Button>} />;
    })}
    <details className="pipeline-evidence"><summary>Inventory history</summary>{pluginHistory.length ? <ul>{pluginHistory.map((entry, index) => <li key={index}>{entry}</li>)}</ul> : <p>No inventory changes in this session.</p>}</details>
  </>} />;
}
