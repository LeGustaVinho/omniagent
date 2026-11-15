import { Activity, CircleDot, FlaskConical, LibraryBig } from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import type { DeliveryStep, WorkspaceProps } from "@/workbench-contract";

const steps: Record<DeliveryStep, string> = {
  note: "Ready to plan", question: "Waiting for input", plan: "Awaiting implementation approval",
  queued: "Queued", running: "Running", review: "In review", verified: "Verified", accepted: "Diff accepted",
  merged: "Merged", interrupted: "Interrupted", failed: "Failed",
};

const inspectorPages = ["Overview", "Timeline", "Agents", "Context", "Exact input", "Approvals", "Artifacts"] as const;
type InspectorPage = (typeof inspectorPages)[number];

export function RunInspector({ runId, simulation, startSimulation, openWorkspace, openChanges }: WorkspaceProps & { runId: string }) {
  const [page, setPage] = useState<InspectorPage>("Overview");
  if (!simulation) return <div className="work-empty">
    <Activity size={22} /><h2>Run inspector</h2><p>Integration pending for this run.</p>
    <Button variant="outline" onClick={() => startSimulation()}>Simulate</Button>
  </div>;

  const step = simulation.delivery;
  const status = steps[step];
  const hasResult = ["review", "verified", "accepted", "merged"].includes(step);

  return (
    <section className="work-inspection">
      <div className="work-inspection-title"><span>Release checklist</span><code>{runId}</code></div>
      <div className="work-tag" role="status"><CircleDot size={13} /> {status}</div>
      <div className="work-inspection-tabs" role="group" aria-label="Run details">
        {inspectorPages.map((name) => <button key={name} type="button" aria-pressed={name === page} onClick={() => setPage(name)}>{name}</button>)}
      </div>
      <div className="work-inspection-body">
        {page === "Overview" && <><h3>Execution</h3><dl>
          <dt>Provider</dt><dd>Codex</dd><dt>Role</dt><dd>Implementation</dd><dt>Worktree</dt><dd><code>example/release-checklist</code></dd>
          <dt>Result</dt><dd>{hasResult ? "Candidate saved" : status}</dd>
        </dl><Button variant="outline" onClick={() => openWorkspace("tasks", "release")}>Open task</Button></>}
        {page === "Timeline" && <><h3>Events</h3><ol className="work-timeline">
          <li><span>Source selected</span><small>docs/release.md · revision 3</small></li>
          {step !== "note" && <li><span>Planning requested</span><small>Read-only profile · Codex</small></li>}
          <li><span>{status}</span><small>Latest state</small></li>
        </ol></>}
        {page === "Agents" && <><h3>Agent tree</h3><ul className="work-timeline">
          <li><span>Planning · Codex</span><small>Read-only workspace</small></li>
          {hasResult && <li className="ml-4"><span>Review · Claude Code</span><small>Independent verification</small></li>}
        </ul></>}
        {page === "Context" && <><h3>Context snapshot</h3><p>Revision 3 · 2,410 input tokens estimated</p>
          <button className="work-text-action" onClick={() => openWorkspace("knowledge", "docs/release.md")}>docs/release.md</button>
          <p>Base commit <code>59a9f0c</code> · checkout instructions included</p></>}
        {page === "Exact input" && <><h3>Exact provider input</h3><pre>Use the repository release checklist revision 3. Keep the task scope limited to the approved changes and capture a diff for review.</pre></>}
        {page === "Approvals" && <><h3>Implementation release</h3><p>{["note", "question", "plan"].includes(step) ? "A decision is required for the selected plan." : "Plan revision 2 · approved example scope"}</p>
          <Button variant="outline" onClick={() => openWorkspace("conversations", "plan")}>Open conversation</Button></>}
        {page === "Artifacts" && <><h3>Artifacts</h3>{hasResult
          ? <Button variant="outline" onClick={() => openChanges("saved")}>Open saved diff</Button>
          : <p>Artifacts appear after the candidate is captured.</p>}</>}
      </div>
    </section>
  );
}

export function SimulationDiagnostics({ openSystem, openActivity, simulation }: WorkspaceProps & { openSystem?: () => void }) {
  const [tab, setTab] = useState<"desktop" | "fixture">("desktop");
  const [fixtureStatus, setFixtureStatus] = useState<"Idle" | "Waiting for input" | "Succeeded" | "Interrupted">("Idle");
  const [input, setInput] = useState("");

  return <section className="work-page" aria-label="Diagnostics workspace">
    <header className="work-page-heading"><div><h1>Diagnostics</h1><p>Desktop runtime and fixture checks</p></div></header>
    <div className="work-inspection-tabs" role="group" aria-label="Diagnostic views">
      <button type="button" aria-pressed={tab === "desktop"} onClick={() => setTab("desktop")}>Desktop</button>
      <button type="button" aria-pressed={tab === "fixture"} onClick={() => setTab("fixture")}>Fixture runs</button>
    </div>
    {tab === "desktop" ? <div className="work-diagnostic-grid">
      <section className="work-stat"><FlaskConical size={20} /><h2>Windows qualification</h2><p>Review provider, WebView2 and installer status for this scenario.</p>
        <Button variant="outline" onClick={openSystem}>System settings</Button></section>
      <section className="work-stat"><LibraryBig size={20} /><h2>Recorded attempt</h2><p>{simulation && steps[simulation.delivery]}</p>
        <Button variant="outline" onClick={() => openActivity("release")}>Open run inspector</Button></section>
    </div> : <section className="work-stat">
      <h2>Process fixture</h2><p role="status">{fixtureStatus}</p>
      <div className="flex flex-wrap items-center gap-2">
        <Button onClick={() => { setFixtureStatus("Waiting for input"); setInput(""); }}>Start fixture</Button>
        {fixtureStatus === "Waiting for input" && <Button variant="outline" onClick={() => setFixtureStatus("Interrupted")}>Cancel run</Button>}
      </div>
      {fixtureStatus === "Waiting for input" && <form className="flex flex-wrap items-end gap-2" onSubmit={(event) => { event.preventDefault(); setFixtureStatus("Succeeded"); }}>
        <label className="grid gap-1">Fixture input<input value={input} onChange={(event) => setInput(event.target.value)} /></label>
        <Button type="submit" disabled={!input.trim()}>Send input</Button>
      </form>}
      {fixtureStatus === "Succeeded" && <pre className="work-fixture-output">Input received: {input}</pre>}
    </section>}
  </section>;
}
