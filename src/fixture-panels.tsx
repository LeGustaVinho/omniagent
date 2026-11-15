import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import type { IDockviewPanelProps } from "dockview-react";
import { useState, type FormEvent } from "react";
import type { FixtureInputEvidence } from "@/bindings/FixtureInputEvidence";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { RunRecord } from "@/bindings/RunRecord";
import type { RunStatus } from "@/bindings/RunStatus";
import { Button } from "@/components/ui/button";
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
        <Button onClick={() => void start()} disabled={starting}>
          {starting ? "Starting…" : "Start fixture"}
        </Button>
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

  function send(event: FormEvent) {
    event.preventDefault();
    void action(async () => {
      await invoke("send_fixture_input", { ...scope, input });
      setInput("");
    });
  }

  const active = run && ["starting", "running", "waiting_for_input"].includes(run.status);
  return (
    <section className="flex h-full flex-col gap-4 overflow-auto p-5">
      <header className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <h1 className="font-mono text-lg font-medium">Run {runId.slice(0, 8)}</h1>
          <p className="mt-1 text-sm text-muted-foreground" role="status">
            {run ? STATUS_LABELS[run.status] : isPending ? "Loading run…" : "Run not found in this checkout"}
          </p>
        </div>
        {active && (
          <Button variant="outline" disabled={pending} onClick={() => void action(() => invoke("cancel_fixture", scope))}>
            Cancel run
          </Button>
        )}
      </header>
      {error && <p role="alert" className="text-sm text-red-300">{error}</p>}
      <ol aria-label="Run events" className="min-h-36 flex-1 overflow-auto rounded-md border border-border bg-background p-3 font-mono text-xs">
        {data?.recentEvents.filter((event) => event.runId === runId).map((event) => (
          <li key={event.seq} className="flex gap-3 py-1.5">
            <span className="w-14 shrink-0 text-muted-foreground">{event.data.type === "status" ? "state" : event.data.stream}</span>
            <span className="min-w-0 break-all">
              {event.data.type === "status" ? STATUS_LABELS[event.data.status] : event.data.preview}
            </span>
          </li>
        ))}
      </ol>
      {run?.status === "waiting_for_input" && (
        <form onSubmit={send} className="flex flex-wrap items-end gap-2">
          <label className="min-w-32 flex-1 text-sm">
            Fixture input
            <input className="mt-1 block w-full rounded-md border border-border bg-background px-3 py-2 font-mono outline-ring" value={input} onChange={(event) => setInput(event.target.value)} />
          </label>
          <Button type="submit" disabled={pending}>Send input</Button>
        </form>
      )}
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
    </section>
  );
}
