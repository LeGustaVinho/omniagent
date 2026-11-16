import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import type { Capability } from "@/bindings/Capability";
import type { Provider } from "@/bindings/Provider";
import type { SessionMode } from "@/bindings/SessionMode";

type CapabilitySubject =
  | { kind: "protocol"; capability: Capability }
  | { kind: "tool"; id: string; version: string; operation: string }
  | { kind: "native_attachment"; format: string; encoding: string }
  | { kind: "attachment_limits" };

type Verification = {
  id: string;
  checkedAt: string;
  origin: "protocol_fixture" | "authenticated_fixture";
  result: "succeeded" | "failed";
  artifactId: string;
  attachmentLimits?: {
    filesPerTurn: number | null;
    encodedRequestBytes: number | null;
    formats: { format: string; encoding: string; fileBytes: number | null; inputTokens: number | null }[];
    context: { windowTokens: number; reservedOutputTokens: number; fixedInputTokens: number } | null;
  };
};

type HistoricalCapability = {
  descriptor: {
    subject: CapabilitySubject;
    source: "adapter" | "provider" | "policy";
    availability: "unknown" | "available" | "unavailable";
    observed: "succeeded" | "failed" | null;
    blocker: "read_only_profile" | null;
    boundary: "equivalent_configuration" | "exact_checkout";
  };
  status: "blocked" | "verified" | "observed" | "reported" | "unverified" | "unavailable";
  evidence: Verification | null;
};

type HistoricalAssessment = {
  version: number;
  transport: "codex_app_server" | "claude_stream_json" | "agent_client_protocol";
  executableRevision: string | null;
  configurationRevision: string | null;
  providerRevision: string | null;
  capabilities: HistoricalCapability[];
};

type ObservedModel = { model: string | null; effort: string | null };

export type DispatchProfileEvidence = {
  dispatchId: string;
  snapshotId: string;
  turnId: string;
  delivery: "prepared" | "submitted" | "unconfirmed";
  profile: {
    provider: Provider;
    profileId: string;
    cliVersion: string;
    runner: string;
    checkoutId: string;
    checkoutPath: string;
    mode: SessionMode;
    permissionRevision: string;
    requestedModel: string | null;
    requestedEffort: string | null;
    reportedAtDispatch: ObservedModel;
  };
  assessment: HistoricalAssessment | null;
  modelObservations: {
    eventSequence: string;
    reported: ObservedModel;
    reassessment: { eventSequence: string; assessment: HistoricalAssessment } | null;
  }[];
};

const providers: Record<Provider, string> = { codex: "Codex", claude: "Claude", antigravity: "Antigravity" };
const transports: Record<HistoricalAssessment["transport"], string> = {
  codex_app_server: "Codex App Server", claude_stream_json: "Claude stream JSON", agent_client_protocol: "Agent Client Protocol",
};
const modes: Record<SessionMode, string> = { read_only: "Read only", plan: "Plan", approved_write: "Approved write" };

function words(value: string) {
  return value.replaceAll("_", " ");
}

function subjectLabel(subject: CapabilitySubject) {
  switch (subject.kind) {
    case "protocol": return words(subject.capability);
    case "tool": return `${subject.id} ${subject.version} · ${subject.operation}`;
    case "native_attachment": return `${words(subject.format)} · ${words(subject.encoding)}`;
    case "attachment_limits": return "Attachment limits";
  }
}

function VerificationDetails({ evidence }: { evidence: Verification }) {
  const limits = evidence.attachmentLimits;
  return <div className="grid gap-1">
    <p>Saved verification · {words(evidence.origin)} · {evidence.result}</p>
    <p>Checked <time dateTime={evidence.checkedAt}>{evidence.checkedAt}</time></p>
    <p className="break-all">Evidence {evidence.id} · Artifact {evidence.artifactId}</p>
    {limits && <>
      {limits.filesPerTurn !== null && <p>Files per turn {limits.filesPerTurn}</p>}
      {limits.encodedRequestBytes !== null && <p>Encoded request {limits.encodedRequestBytes} bytes</p>}
      {limits.formats.length > 0 && <ul>{limits.formats.map((format) => <li key={`${format.format}:${format.encoding}`}>
        {words(format.format)} · {words(format.encoding)}
        {format.fileBytes !== null && ` · ${format.fileBytes} bytes per file`}
        {format.inputTokens !== null && ` · ${format.inputTokens} input tokens per file`}
      </li>)}</ul>}
      {limits.context && <p>Context {limits.context.windowTokens} tokens · Output reserve {limits.context.reservedOutputTokens}
        {` · Fixed input ${limits.context.fixedInputTokens}`}</p>}
    </>}
  </div>;
}

function AssessmentDetails({ assessment }: { assessment: HistoricalAssessment }) {
  return <div className="grid gap-2">
    <dl className="break-all">
      <dt>Transport</dt><dd>{transports[assessment.transport]}</dd>
      {assessment.executableRevision && <><dt>Executable revision</dt><dd>{assessment.executableRevision}</dd></>}
      {assessment.configurationRevision && <><dt>Effective configuration revision</dt><dd>{assessment.configurationRevision}</dd></>}
      {assessment.providerRevision && <><dt>Provider configuration revision</dt><dd>{assessment.providerRevision}</dd></>}
    </dl>
    <ul aria-label="Capability evidence" className="grid gap-1">
      {assessment.capabilities.map(({ descriptor, status, evidence }) => <li key={JSON.stringify(descriptor.subject)}>
        <details className="rounded border border-border px-2 py-1">
          <summary className="cursor-pointer break-words capitalize">{subjectLabel(descriptor.subject)} · {status}</summary>
          <div className="grid gap-1 pt-1 text-muted-foreground">
            <p>Source {descriptor.source} · Availability {descriptor.availability} · Scope {words(descriptor.boundary)}</p>
            {descriptor.observed && <p>Observed result {descriptor.observed}</p>}
            {descriptor.blocker && <p>Blocked by the read-only profile</p>}
            {evidence && <VerificationDetails evidence={evidence} />}
          </div>
        </details>
      </li>)}
    </ul>
  </div>;
}

function ProfileDetails({ evidence }: { evidence: DispatchProfileEvidence }) {
  const { profile, assessment, modelObservations } = evidence;
  return <>
    <section className="grid gap-2" aria-label="Effective profile at dispatch">
      <dl className="break-all">
        <dt>Agent profile</dt><dd>{providers[profile.provider]} · {profile.profileId}</dd>
        <dt>Requested model</dt><dd>{profile.requestedModel ?? "Provider default"}</dd>
        {profile.requestedEffort && <><dt>Requested effort</dt><dd>{profile.requestedEffort}</dd></>}
        <dt>Reported model at dispatch</dt><dd>{profile.reportedAtDispatch.model ?? "Unavailable"}</dd>
        {profile.reportedAtDispatch.effort && <><dt>Reported effort at dispatch</dt><dd>{profile.reportedAtDispatch.effort}</dd></>}
        <dt>CLI version</dt><dd>{profile.cliVersion}</dd>
        <dt>Runner</dt><dd>{profile.runner}</dd>
        <dt>Checkout</dt><dd>{profile.checkoutPath} · {profile.checkoutId}</dd>
        <dt>Permissions</dt><dd>{modes[profile.mode]} · {profile.permissionRevision}</dd>
        <dt>Dispatch</dt><dd>{evidence.dispatchId} · {evidence.delivery}</dd>
        <dt>Context snapshot</dt><dd>{evidence.snapshotId}</dd>
      </dl>
      {assessment ? <AssessmentDetails assessment={assessment} /> : <p>Capability assessment unavailable.</p>}
    </section>
    {modelObservations.length > 0 && <section aria-label="Provider model reports after dispatch" className="grid gap-2">
      <h3 className="text-sm font-medium">Provider model reports after dispatch</h3>
      <ol className="grid gap-2">{modelObservations.map(({ eventSequence, reported, reassessment }) => <li key={eventSequence} className="grid gap-1">
        <p className="break-words">{reported.model ?? "Model unavailable"}{reported.effort && ` · Effort ${reported.effort}`} · Event {eventSequence}</p>
        {reassessment && <details>
          <summary className="cursor-pointer">Assessment after report · Event {reassessment.eventSequence}</summary>
          <AssessmentDetails assessment={reassessment.assessment} />
        </details>}
      </li>)}</ol>
    </section>}
  </>;
}

export function ConversationProfile({ projectId, conversationId, runId, live }: {
  projectId: string;
  conversationId: string;
  runId: string;
  live: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const evidence = useQuery({
    queryKey: ["conversation-profile-evidence", projectId, conversationId, runId, live],
    enabled: expanded,
    refetchInterval: ({ state }) => expanded && live && (!state.data || state.data.delivery === "prepared") ? 1250 : false,
    queryFn: () => invoke<DispatchProfileEvidence | null>("conversation_profile_evidence", { projectId, conversationId, runId }),
  });
  return <details onToggle={(event) => setExpanded(event.currentTarget.open)} className="min-w-0">
    <summary className="cursor-pointer">Profile and capabilities</summary>
    {expanded && <div className="grid gap-3 pt-2 text-xs">
      {evidence.isPending && <p role="status">Loading dispatch profile…</p>}
      {evidence.error && <p role="alert">{String(evidence.error)}</p>}
      {evidence.data === null && <p>Dispatch profile unavailable.</p>}
      {evidence.data && <ProfileDetails evidence={evidence.data} />}
    </div>}
  </details>;
}
