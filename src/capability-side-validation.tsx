import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { Button } from "@/components/ui/button";
import {
  sideUnavailableReason, useSideRequests, type OutputFixture, type OutputFixtureReceipt, type SideRequest, type SideSubmission,
} from "@/conversation-side-requests";

export type SideValidationTarget = {
  conversationId: string;
  sourceRunId: string;
  selection: ModelSelection;
  effort: string | null;
  mode: "read_only" | "plan";
};

function isPending(request: SideRequest) {
  const { job } = request;
  return job.queueState !== "cancelled" && (!job.latestAttempt || job.queuedRecovery ||
    ["starting", "running", "waiting_for_input"].includes(job.latestAttempt.status));
}

const fixtures = {
  audio_generation: { name: "Audio generation", target: "Generate the fixed verification phrase with Windows speech synthesis." },
  browser_control: { name: "Browser control", target: "Activate the button on a local test page in a disposable browser profile." },
  host_control: { name: "Windows host control", target: "Change the caption of an isolated Windows test window." },
};

function AudioPreview({ projectId, conversationId, runId }: { projectId: string; conversationId: string; runId: string }) {
  const [open, setOpen] = useState(false);
  const query = useQuery({
    queryKey: ["output-fixture-audio", projectId, conversationId, runId],
    enabled: open,
    queryFn: () => invoke<string | null>("conversation_output_fixture_audio", { projectId, conversationId, runId }),
  });
  return <details onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary className="cursor-pointer">Preview generated audio</summary>
    {open && query.isPending && <p role="status">Loading generated audio…</p>}
    {open && query.error && <p role="alert">{String(query.error)}</p>}
    {open && query.data === null && <p>Saved audio preview unavailable.</p>}
    {open && query.data && <audio controls aria-label="Generated audio" src={`data:audio/wav;base64,${query.data}`} />}
  </details>;
}

function OutputReceipt({ receipt, record, conversationId }: { receipt: OutputFixtureReceipt; record: SideRequest; conversationId: string }) {
  const result = receipt.result;
  const attempt = record.job.latestAttempt;
  return <>
    <dl className="grid gap-1 break-words">
      <dt>Producer</dt><dd>OmniAgent-managed Windows tool</dd>
      <dt>Tool</dt><dd>{receipt.tool} · revision {receipt.revision}</dd>
      <dt>CLI tool call</dt><dd>{receipt.providerCallId}</dd>
      {result.kind === "audio_generation" && <>
        <dt>Voice</dt><dd>{result.voice}</dd>
        <dt>Generated phrase</dt><dd>{result.phrase}</dd>
        <dt>Size</dt><dd>{result.byteLen.toLocaleString()} bytes</dd>
        <dt>SHA-256</dt><dd>{result.sha256}</dd>
      </>}
      {result.kind === "browser_control" && <>
        <dt>Browser</dt><dd>{result.browser}</dd>
        <dt>Tested target</dt><dd>{result.target}</dd>
        <dt>Action</dt><dd>{result.operation}</dd>
        <dt>Observed result</dt><dd>{result.result}</dd>
      </>}
      {result.kind === "host_control" && <>
        <dt>Tested target</dt><dd>Disposable Windows window</dd>
        <dt>Action</dt><dd>{result.operation}</dd>
        <dt>Before</dt><dd>{result.before}</dd>
        <dt>Observed result</dt><dd>{result.after}</dd>
      </>}
    </dl>
    {result.kind === "audio_generation" && attempt && <AudioPreview projectId={record.job.projectId} conversationId={conversationId} runId={attempt.runId} />}
  </>;
}

function FixtureResult({ record, conversationId }: { record: SideRequest; conversationId: string }) {
  const outcome = record.response?.outcome;
  const attempt = record.job.latestAttempt;
  let result: string = attempt?.status ?? "Unavailable";
  if (isPending(record)) result = attempt ? "Running" : "Queued";
  else if (outcome?.kind === "unavailable") result = sideUnavailableReason(outcome.reason);
  else if (record.job.queueState === "cancelled") result = "Cancelled";
  else if (attempt?.status === "succeeded") result = record.evidenceId || record.response?.outputFixture ? "Passed" : "Evidence unavailable";
  else if (outcome?.kind === "timed_out") result = "Timed out";
  return <>
  <dl className="grid gap-1 break-words">
    <dt>Validation target</dt><dd>{record.outputFixture ? fixtures[record.outputFixture].name : "Isolated side request"} · {record.provider} · {record.profileId} · {record.model ?? "Provider default"}</dd>
    <dt>Result</dt><dd role="status" className="capitalize">{result}</dd>
    <dt>Queue job</dt><dd>{record.job.id}</dd>
    {attempt && <><dt>Attempt</dt><dd>{attempt.runId}</dd></>}
    {outcome?.kind === "answered" && outcome.route && <><dt>Observed route</dt><dd>{outcome.route.replaceAll("_", " ")}</dd></>}
    {record.evidenceId && <><dt>Evidence</dt><dd>{record.evidenceId}</dd></>}
    {record.evidenceCheckedAt && <><dt>Checked</dt><dd>
      <time dateTime={record.evidenceCheckedAt}>{record.evidenceCheckedAt}</time>
    </dd></>}
  </dl>
  {record.response?.outputFixture && <OutputReceipt receipt={record.response.outputFixture} record={record} conversationId={conversationId} />}
  </>;
}

export function SideFixtureEvidence({ conversationId, evidenceId, attemptId }: {
  conversationId: string;
  evidenceId: string;
  attemptId: string;
}) {
  const [open, setOpen] = useState(false);
  const records = useSideRequests(conversationId, open);
  const fixture = records.data?.find((record) => record.purpose === "validation"
    && record.evidenceId === evidenceId && record.job.latestAttempt?.runId === attemptId);
  return <details onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary className="cursor-pointer underline">View fixture evidence</summary>
    {open && <>
      {records.isPending && <p role="status">Loading fixture evidence…</p>}
      {records.error && <p role="alert">{String(records.error)}</p>}
      {fixture ? <FixtureResult record={fixture} conversationId={conversationId} /> : records.isSuccess && <p>The fixture attempt is unavailable in this conversation.</p>}
    </>}
  </details>;
}

export function SideCapabilityValidation({ target }: { target: SideValidationTarget }) {
  const client = useQueryClient();
  const [showHistory, setShowHistory] = useState(false);
  const [fixture, setFixture] = useState<OutputFixture | "side_request">("side_request");
  const selected = target.selection.provider === "codex" ? fixture : "side_request";
  const launch = useMutation({
    mutationFn: (selected: OutputFixture | "side_request") => invoke<SideSubmission>("enqueue_conversation_side_request", {
      conversationId: target.conversationId, expectedContextId: null, expectedSourceRunId: target.sourceRunId,
      selection: target.selection, effort: target.effort, mode: target.mode, purpose: "validation", question: "",
      ...(selected === "side_request" ? {} : { outputFixture: selected }),
    }),
    onSuccess: async (result) => {
      if (result.status !== "queued") return;
      setShowHistory(true);
      await Promise.all([
        client.invalidateQueries({ queryKey: ["conversation", target.conversationId, "side-requests"] }),
        client.invalidateQueries({ queryKey: ["project-job-queue"] }),
      ]);
    },
  });
  const records = useSideRequests(target.conversationId, showHistory);
  const history = records.data?.filter((record) =>
    record.purpose === "validation" && record.sourceRunId === target.sourceRunId) ?? [];
  const launchedJob = launch.data?.status === "queued" ? launch.data.jobId : undefined;
  const active = history.some(isPending) || Boolean(launchedJob && records.isFetching && !history.some(({ job }) => job.id === launchedJob));
  return <section className="grid gap-2 py-1" aria-label="Side request capability validation">
    {target.selection.provider === "codex" && <label className="grid gap-1">
      Capability to validate
      <select value={fixture} onChange={(event) => setFixture(event.currentTarget.value as OutputFixture | "side_request")}>
        <option value="side_request">Isolated side request</option>
        {Object.entries(fixtures).map(([id, { name }]) => <option key={id} value={id}>{name} · Managed tool</option>)}
      </select>
    </label>}
    <div className="flex flex-wrap items-center gap-2">
      <Button type="button" variant="outline" disabled={launch.isPending || active} onClick={() => launch.mutate(selected)}>
        Validate capability
      </Button>
      {!showHistory && <button type="button" className="underline" onClick={() => setShowHistory(true)}>Validation history</button>}
    </div>
    <p>{selected === "side_request" ? "Runs an isolated side-request fixture through the recorded profile." : fixtures[selected].target} The check can consume account usage.</p>
    {launch.error && <p role="alert">{String(launch.error)}</p>}
    {launch.data?.status === "unavailable" && <p role="status">{sideUnavailableReason(launch.data.reason)}</p>}
    {launch.data?.status === "skipped" && <p role="status">The conversation’s eligible source turn changed.</p>}
    {showHistory && <>
      {records.error && <p role="alert">{String(records.error)}</p>}
      {records.isPending && <p role="status">{launchedJob ? "Queued for validation…" : "Loading validation history…"}</p>}
      {records.isSuccess && !history.length && <p>{launchedJob
        ? records.isFetching ? "Queued for validation…" : "The queued validation record is unavailable."
        : "No validation attempts for this source turn."}</p>}
      {history.map((record) => <details key={record.job.id} open={record.job.id === launchedJob}>
        <summary className="cursor-pointer">Validation evidence · {record.job.id}</summary>
        <FixtureResult record={record} conversationId={target.conversationId} />
      </details>)}
    </>}
  </section>;
}
