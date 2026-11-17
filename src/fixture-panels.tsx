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
import { DiagnosticsCard } from "@/diagnostics-workspace";
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

type FixtureEvent = Pick<RunEvent, "seq" | "data">;
type FixtureInput = { value: string; onChange: (value: string) => void; onSend: () => void; disabled?: boolean };
type FixtureProcessProps = {
  title: string;
  heading?: "h1" | "h2";
  status: string;
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

function FixtureProcess({ title, heading, status, events, error, pending, start, cancel, input, children }: FixtureProcessProps) {
  return <DiagnosticsCard title={title} icon={FlaskConical} heading={heading}>
    <div className="grid gap-3">
      <p role="status">{status}</p>
      {error && <p role="alert" className="text-destructive">{error}</p>}
      {(start || cancel) && <div className="flex flex-wrap items-center gap-2">
        {start && <StartFixtureButton start={start} pending={pending} />}
        {cancel && <Button type="button" variant="outline" onClick={cancel} disabled={pending}>Cancel run</Button>}
      </div>}
      {events && <ol aria-label="Run events" className="max-h-96 min-h-36 overflow-auto rounded-md border border-border bg-background p-3 font-mono text-xs">
        {events.map(({ seq, data }) => <li key={seq} className="flex gap-3 py-1.5">
          <span className="w-14 shrink-0 text-muted-foreground">{data.type === "status" ? "state" : data.stream}</span>
          <span className="min-w-0 whitespace-pre-wrap [overflow-wrap:anywhere]">
            {data.type === "status" ? STATUS_LABELS[data.status] : data.preview}
          </span>
        </li>)}
      </ol>}
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

export function SimulationFixture() {
  const [status, setStatus] = useState<"idle" | "waiting_for_input" | "succeeded" | "cancelled">("idle");
  const [input, setInput] = useState("");
  const events: FixtureEvent[] = [];
  if (status !== "idle") events.push({ seq: "1", data: { type: "status", status: "waiting_for_input" } });
  if (status === "succeeded") {
    events.push({ seq: "2", data: { type: "output", stream: "stdout", preview: `Input received: ${input}` } });
  }
  if (status === "succeeded" || status === "cancelled") events.push({ seq: "3", data: { type: "status", status } });

  return <FixtureProcess title="Process fixture" status={status === "idle" ? "Idle" : STATUS_LABELS[status]}
    events={status === "idle" ? undefined : events}
    start={status !== "waiting_for_input" ? () => { setInput(""); setStatus("waiting_for_input"); } : undefined}
    cancel={status === "waiting_for_input" ? () => setStatus("cancelled") : undefined}
    input={status === "waiting_for_input" ? {
      value: input, onChange: setInput, onSend: () => setStatus("succeeded"), disabled: !input.trim(),
    } : undefined} />;
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
      {isPending && <p role="status" className="text-sm text-muted-foreground">Loading runs…</p>}
      {data?.runs.length === 0 && <p className="text-sm text-muted-foreground">No fixture runs in this checkout.</p>}
      {data && data.runs.length > 0 && (
        <ul className="divide-y divide-border rounded-lg border border-border">
          {data.runs.map((run) => (
            <li key={run.id}>
              <button type="button" className="flex w-full items-center justify-between gap-4 px-4 py-3 text-left hover:bg-muted" onClick={() => onOpen(run.id)}>
                <span className="font-mono text-sm">Run {run.id.slice(0, 8)}</span>
                <span className="text-xs text-muted-foreground">{STATUS_LABELS[run.status]}</span>
              </button>
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
  return <FixtureRun checkoutId={checkout.checkoutId} runId={params.runId} />;
}

function FixtureRun({ checkoutId, runId }: { checkoutId: string; runId: string }) {
  const { data, isPending } = useQuery(fixtureProjection(checkoutId));
  const run = data?.runs.find((candidate) => candidate.id === runId);
  const [input, setInput] = useState("");
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);
  const [evidence, setEvidence] = useState<FixtureInputEvidence | null>();
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
      <FixtureProcess title={`Run ${runId.slice(0, 8)}`} heading="h1"
        status={run ? STATUS_LABELS[run.status] : isPending ? "Loading run…" : "Run not found in this checkout"}
        events={data?.recentEvents.filter((event) => event.runId === runId)} error={error} pending={pending}
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
                <pre className="mt-2 whitespace-pre-wrap break-all text-xs">{evidence.text}</pre>
              </div>
            )}
          </div>
        )}
      </FixtureProcess>
    </div>
  );
}
