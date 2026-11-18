import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import type { IDockviewPanelProps } from "dockview-react";
import { FlaskConical } from "lucide-react";
import { useState, type ReactNode } from "react";
import type { FixtureInputEvidence } from "@/bindings/FixtureInputEvidence";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { RunEvent } from "@/bindings/RunEvent";
import type { RunRecord } from "@/bindings/RunRecord";
import type { RunStatus } from "@/bindings/RunStatus";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { DiagnosticsCard } from "@/diagnostics-workspace";
import { downloadArtifact } from "@/download-artifact";
import { fixtureProjection } from "@/run-projection";
import { openFixtureRun } from "@/workspace-tabs";
import { useWorkspace } from "@/workspace-state";

const STATUS_LABELS: Record<RunStatus, string> = {
  starting: "Starting",
  running: "Running",
  waiting_for_input: "Waiting for input",
  succeeded: "Succeeded",
  cancelled: "Cancelled",
  failed: "Failed",
  interrupted: "Interrupted",
};

type FixtureEvent = Pick<RunEvent, "seq" | "data"> & Partial<Pick<RunEvent, "runId" | "turnId">>;
type FixtureChannel = "status" | "stdout" | "stderr";
type FixtureFilter = "all" | FixtureChannel;
const filterLabels: Record<FixtureFilter, string> = {
  all: "All events", stdout: "Standard output (stdout)", stderr: "Standard error (stderr)", status: "Status transitions",
};
const filters: FixtureFilter[] = ["all", "stdout", "stderr", "status"];
type FixtureInput = { value: string; onChange: (value: string) => void; onSend: () => void; disabled?: boolean };
type FixtureProcessProps = {
  title: string;
  heading?: "h1" | "h2";
  status: string;
  checkoutId?: string;
  runId?: string;
  events?: FixtureEvent[];
  error?: string;
  pending?: boolean;
  start?: () => void;
  cancel?: () => void;
  input?: FixtureInput;
  children?: ReactNode;
};

function StartFixtureButton({ start, pending }: { start: () => void; pending?: boolean }) {
  return <Button type="button" onClick={start} disabled={pending}>{pending ? "Starting…" : "Start fixture"}</Button>;
}

function FixtureProcess({ title, heading, status, checkoutId, runId, events, error, pending, start, cancel, input, children }: FixtureProcessProps) {
  const { copy, feedback, run: perform } = useContextActionFeedback();
  const [filter, setFilter] = useState<FixtureFilter>("all");
  const entries = events?.map((event) => {
    const { data } = event;
    return data.type === "status"
      ? { event, channel: "status" as const, display: "state", text: STATUS_LABELS[data.status] }
      : { event, channel: data.stream, display: data.stream, text: data.preview };
  });
  const visible = entries?.filter(({ channel }) => filter === "all" || filter === channel);
  const captured = () => JSON.stringify({
    source: "fixture_recent_events",
    checkoutId: checkoutId ?? null,
    runId: runId ?? null,
    filter,
    output: "redacted_preview",
    events: visible?.map(({ event }) => event) ?? [],
  }, null, 2);
  const range = filter === "all" ? "recent" : "visible";
  return <DiagnosticsCard title={title} icon={FlaskConical} heading={heading} menu={[[
    ...(runId ? [{ label: "Copy fixture run ID", run: () => copy(runId) }] : []),
    ...(visible?.length ? [
      { label: `Copy ${range} fixture events`, run: () => copy(
        visible.map(({ event, display, text }) => `[${event.seq} ${display}] ${text}`).join("\n"),
      ) },
      { label: `Copy ${range} fixture events as JSON`, run: () => copy(captured()) },
      { label: `Download ${range} fixture events (.json)`, run: () => void perform(async () => {
        const filename = `fixture-${runId}-events`;
        downloadArtifact(`${filename}-${filter}.json`, new Blob([captured()], { type: "application/json" }));
      }) },
    ] : []),
  ], [
    ...(entries?.length ? [{
      label: "Filter events", items: filters.filter((channel) => channel !== filter)
        .map((channel) => ({ label: filterLabels[channel], run: () => setFilter(channel) })),
    }] : []),
  ]]}>
    <div className="grid gap-3">
      <p role="status">{status}</p>
      {error && <p role="alert" className="text-destructive">{error}</p>}
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      {(start || cancel) && <div className="flex flex-wrap items-center gap-2">
        {start && <StartFixtureButton start={start} pending={pending} />}
        {cancel && <Button type="button" variant="outline" onClick={cancel} disabled={pending}>Cancel run</Button>}
      </div>}
      {visible && <div className="grid gap-2">
        <label className="flex flex-wrap items-center gap-2 text-sm">Fixture event filter
          <select className="rounded-md border border-input bg-background px-2 py-1" value={filter}
            onChange={(event) => setFilter(event.currentTarget.value as FixtureFilter)}>
            {filters.map((channel) => <option key={channel} value={channel}>{filterLabels[channel]}</option>)}
          </select>
        </label>
        <p className="text-xs text-muted-foreground">Recent events for this checkout and run. Output previews may be redacted or shortened, and older events may be omitted.</p>
        <ol aria-label="Run events" className="max-h-96 min-h-36 overflow-auto rounded-md border border-border bg-background p-3 font-mono text-xs">
          {visible.map(({ event, channel, display, text }) => <li key={event.seq}>
            <ContextActions label={`${display} event ${event.seq}`} groups={[[
              { label: "Copy event text", disabled: !text, run: () => copy(text) },
              { label: "Copy event sequence", run: () => copy(event.seq) },
              { label: "Copy event as JSON", run: () => copy(JSON.stringify(event, null, 2)) },
            ], [
              filter === channel ? { label: "Show all events", run: () => setFilter("all") }
                : { label: `Show only ${channel} events`, run: () => setFilter(channel) },
            ]]}>
              <div className="flex min-w-0 gap-3 py-1.5">
                <span className="w-14 shrink-0 text-muted-foreground">{display}</span>
                <span className="min-w-0 whitespace-pre-wrap [overflow-wrap:anywhere]">{text}</span>
              </div>
            </ContextActions>
          </li>)}
          {visible.length === 0 && <li className="text-muted-foreground">No {filter === "all" ? "recent" : filter} events.</li>}
        </ol>
      </div>}
      {input && <form className="flex flex-wrap items-end gap-2" onSubmit={(event) => {
        event.preventDefault();
        if (!pending && !input.disabled) input.onSend();
      }}>
        <label className="grid min-w-32 flex-1 gap-1">Fixture input
          <input className="min-w-0 rounded-md border border-input bg-background px-3 py-2 font-mono outline-ring"
            value={input.value} onChange={(event) => input.onChange(event.target.value)} />
        </label>
        <Button type="submit" disabled={pending || input.disabled}>Send input</Button>
      </form>}
      {children}
    </div>
  </DiagnosticsCard>;
}

export function FixtureListPanel({ containerApi }: IDockviewPanelProps) {
  const checkout = useWorkspace((state) => state.checkout);
  if (!checkout) return null;
  return <FixtureList checkout={checkout} onOpen={(id) => openFixtureRun(containerApi, id)} />;
}

function FixtureList({ checkout, onOpen }: { checkout: ProjectCheckout; onOpen: (id: string) => void }) {
  const { checkoutId } = checkout;
  const { data, isPending } = useQuery(fixtureProjection(checkoutId));
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState("");
  const { copy, feedback } = useContextActionFeedback();

  async function start() {
    setStarting(true);
    setError("");
    try {
      const run = await invoke<RunRecord>("start_fixture", { checkoutId });
      onOpen(run.id);
    } catch (error) {
      setError(String(error));
    } finally {
      setStarting(false);
    }
  }

  return (
    <section className="h-full overflow-auto px-6 py-6">
      <header className="mb-6 flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-xl font-semibold tracking-tight">Fixture runs</h1>
        <StartFixtureButton start={() => void start()} pending={starting} />
      </header>
      {error && <p role="alert" className="mb-3 text-sm text-red-300">{error}</p>}
      {feedback && <p role={feedback.failed ? "alert" : "status"} className="mb-3 text-sm">{feedback.text}</p>}
      {isPending && <p role="status" className="text-sm text-muted-foreground">Loading runs…</p>}
      {data?.runs.length === 0 && <p className="text-sm text-muted-foreground">No fixture runs in this checkout.</p>}
      {data && data.runs.length > 0 && (
        <ul className="divide-y divide-border rounded-lg border border-border">
          {data.runs.map((run) => (
            <li key={run.id}>
              <ContextActions label={`Fixture run ${run.id}`} className="pr-2" groups={[[
                { label: "Open fixture run", run: () => onOpen(run.id) },
                { label: "Copy fixture run ID", run: () => copy(run.id) },
                { label: "Copy fixture status", run: () => copy(STATUS_LABELS[run.status]) },
              ]]}>
                <button type="button" className="flex w-full items-center justify-between gap-4 px-4 py-3 text-left hover:bg-muted" onClick={() => onOpen(run.id)}>
                  <span className="font-mono text-sm">Run {run.id.slice(0, 8)}</span>
                  <span className="text-xs text-muted-foreground">{STATUS_LABELS[run.status]}</span>
                </button>
              </ContextActions>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

export function FixtureRunPanel({ params }: IDockviewPanelProps<{ runId: string }>) {
  const checkout = useWorkspace((state) => state.checkout);
  if (!checkout) return null;
  return <FixtureRun key={JSON.stringify([checkout.checkoutId, params.runId])} checkoutId={checkout.checkoutId} runId={params.runId} />;
}

function FixtureRun({ checkoutId, runId }: { checkoutId: string; runId: string }) {
  const { data, isPending } = useQuery(fixtureProjection(checkoutId));
  const run = data?.runs.find((candidate) => candidate.id === runId);
  const [input, setInput] = useState("");
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);
  const [evidence, setEvidence] = useState<FixtureInputEvidence | null>();
  const { copy, feedback } = useContextActionFeedback();
  const scope = { checkoutId, runId };

  async function action(callback: () => Promise<unknown>) {
    setError("");
    setPending(true);
    try {
      await callback();
    } catch (error) {
      setError(String(error));
    } finally {
      setPending(false);
    }
  }

  function send() {
    void action(async () => {
      await invoke("send_fixture_input", { ...scope, input });
      setInput("");
    });
  }

  const active = run && ["starting", "running", "waiting_for_input"].includes(run.status);
  return (
    <div className="h-full overflow-auto p-5">
      <FixtureProcess title={`Run ${runId.slice(0, 8)}`} heading="h1" checkoutId={checkoutId} runId={run?.id}
        status={run ? STATUS_LABELS[run.status] : isPending ? "Loading run…" : "Run not found in this checkout"}
        events={run ? data?.recentEvents.filter((event) => event.runId === runId) : undefined} error={error} pending={pending}
        cancel={active ? () => void action(() => invoke("cancel_fixture", scope)) : undefined}
        input={run?.status === "waiting_for_input" ? { value: input, onChange: setInput, onSend: send } : undefined}>
        {run && (
          <div className="text-sm">
            <Button variant="outline" disabled={pending} onClick={() => {
              if (evidence !== undefined) setEvidence(undefined);
              else void action(async () => setEvidence(await invoke<FixtureInputEvidence | null>("fixture_input_evidence", scope)));
            }}>
              {evidence === undefined ? "Inspect protected input" : "Hide protected input"}
            </Button>
            {evidence === null && <p className="mt-2 text-muted-foreground">No input recorded.</p>}
            {evidence && (
              <div className="mt-2 rounded-md border border-border p-3">
                <p className="text-xs text-muted-foreground">{evidence.sent ? "Sent to fixture" : "Prepared input"}</p>
                <ContextActions label="Displayed fixture input" groups={[[
                  { label: "Copy displayed input", run: () => copy(evidence.text) },
                ]]}><pre className="mt-2 min-w-0 whitespace-pre-wrap break-all text-xs">{evidence.text}</pre></ContextActions>
              </div>
            )}
            {feedback && <p role={feedback.failed ? "alert" : "status"} className="mt-2">{feedback.text}</p>}
          </div>
        )}
      </FixtureProcess>
    </div>
  );
}
