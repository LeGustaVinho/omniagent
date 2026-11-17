import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { useRef, useState, type ReactNode, type Ref } from "react";
import type { Provider } from "@/bindings/Provider";
import type { SessionMode } from "@/bindings/SessionMode";
import { capabilitySubjectLabel, type CapabilitySubject } from "@/capability-subject";
import { SideCapabilityValidation, type SideValidationTarget } from "@/capability-side-validation";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";

type WindowsVersion = { major: number; minor: number; build: number; revision: number; nativeArchitecture: number; server: boolean };
type Result = "succeeded" | "failed";
type RecordedCapability = {
  subject: CapabilitySubject;
  boundary: "equivalent_configuration" | "exact_checkout";
  status: "blocked" | "verified" | "fixture_passed" | "observed" | "reported" | "unverified" | "unavailable";
  observed: Result | null;
  evidence: { origin: "protocol_fixture" | "authenticated_fixture" | "authenticated_provider_run"; checkedAt: string; result: Result } | null;
};

type RecordedProfile = {
  runId: string;
  recordedAt: number;
  provider: Provider;
  cliVersion: string;
  requestedModel: string | null;
  reportedModel: string | null;
  platform: { host: WindowsVersion; guest: WindowsVersion | null } | null;
  runner: string;
  mode: SessionMode;
  capabilities: RecordedCapability[];
  validation: SideValidationTarget | null;
};

type Cursor = Pick<RecordedProfile, "recordedAt" | "runId">;
type QualificationPage = { records: RecordedProfile[]; nextCursor: Cursor | null };
type MatrixRow = { id: string; cli: ReactNode; model: ReactNode; host: ReactNode; runner: ReactNode; qualification: ReactNode };

const providers: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Antigravity" };
const modes: Record<SessionMode, string> = { read_only: "Read only", plan: "Plan", approved_write: "Approved write" };
const statuses: Record<RecordedCapability["status"], string> = {
  blocked: "Blocked", verified: "Verified in provider run", fixture_passed: "Fixture passed", observed: "Observed", reported: "Reported",
  unverified: "Unverified", unavailable: "Unavailable",
};
const origins: Record<NonNullable<RecordedCapability["evidence"]>["origin"], string> = {
  protocol_fixture: "Protocol fixture", authenticated_fixture: "Authenticated fixture", authenticated_provider_run: "Authenticated provider run",
};

const exampleRows: MatrixRow[] = [
  { id: "codex", cli: "Codex", model: "GPT-6.1 Sol", host: "Windows 11", runner: "Direct", qualification: "Verified · Read-only and writing" },
  { id: "claude", cli: "Claude Code", model: "Claude Sonnet 5.5", host: "Windows 11", runner: "Direct", qualification: "Verified · Approval prompts" },
  { id: "antigravity", cli: "Antigravity", model: "Gemini 4 Argon", host: "Windows 10 22H2", runner: "Hyper-V guest", qualification: "Blocked · Pairing required" },
];

const displayDate = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const windowsArchitectures: Partial<Record<number, string>> = { 0: "x86", 9: "x64", 12: "ARM64" };

function windowsVersion({ major, minor, build, revision, nativeArchitecture, server }: WindowsVersion) {
  const arch = windowsArchitectures[nativeArchitecture];
  const name = server ? "Windows Server" : "Windows";
  return `${name} ${major}.${minor}.${build}.${revision}${arch ? ` · ${arch}` : ""}`;
}

function RecordedCapabilities({ capabilities, disclosure }: { capabilities: RecordedCapability[]; disclosure: Ref<HTMLDetailsElement> }) {
  const { copy, feedback } = useContextActionFeedback();
  return <details ref={disclosure} className="min-w-32">
    <summary>Recorded capabilities</summary>
    {capabilities.length ? <ul className="grid max-w-72 gap-2 py-2 whitespace-normal [overflow-wrap:anywhere]" aria-label="Recorded capabilities">
      {capabilities.map(({ subject, boundary, status, observed, evidence }) => {
        const capability = capabilitySubjectLabel(subject);
        const title = `${capability} · ${statuses[status]}`;
        const scope = boundary === "exact_checkout" ? "Exact checkout" : "Equivalent configuration";
        return <li key={JSON.stringify(subject)}><ContextActions label={title} groups={[[
          { label: "Copy capability assessment", run: () => copy([
            title, scope, observed && `Observed result · ${observed}`,
            evidence && `${origins[evidence.origin]} · ${evidence.result} · ${evidence.checkedAt}`,
          ].filter(Boolean).join("\n")) },
        ]]}>
          <div className="min-w-0">
            <strong>{title}</strong>
            <div className="text-muted-foreground">{scope}</div>
            {observed && <div>Observed result · {observed}</div>}
            {evidence && <div>{origins[evidence.origin]} · {evidence.result} · <time dateTime={evidence.checkedAt}>{displayDate.format(new Date(evidence.checkedAt))}</time></div>}
          </div>
        </ContextActions></li>;
      })}
    </ul> : <p>No capability assessment recorded.</p>}
    {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
  </details>;
}

function RecordedValidation({ target }: { target: SideValidationTarget }) {
  const [expanded, setExpanded] = useState(false);
  return <details className="mt-2 max-w-72 whitespace-normal [overflow-wrap:anywhere]"
    onToggle={(event) => setExpanded(event.currentTarget.open)}>
    <summary>Validate capability</summary>
    {expanded && <SideCapabilityValidation target={target} />}
  </details>;
}

function RecordedCli({ record, showQualification }: { record: RecordedProfile; showQualification: () => void }) {
  const { copy, feedback } = useContextActionFeedback();
  const recordedAt = new Date(record.recordedAt * 1000);
  const { runId, cliVersion, provider, reportedModel, requestedModel, platform, runner, mode } = record;
  const title = `${providers[provider]} ${cliVersion}`;
  return <>
    <ContextActions label={`Recorded profile ${runId}`} groups={[[
      { label: "Show qualification details", run: showQualification },
    ], [
      { label: "Copy qualification run ID", run: () => copy(runId) },
      { label: "Copy recorded profile", run: () => copy([
        title, `Run ${runId}`, `Recorded ${recordedAt.toISOString()}`,
        requestedModel && `Requested model · ${requestedModel}`, reportedModel && `Reported model · ${reportedModel}`,
        platform && `Host · ${windowsVersion(platform.host)}`, platform?.guest && `Guest · ${windowsVersion(platform.guest)}`,
        `${runner} · ${modes[mode]}`,
      ].filter(Boolean).join("\n")) },
    ]]}>
      <div className="min-w-0"><strong>{providers[provider]}</strong> {cliVersion}<br />
        <time className="text-muted-foreground" dateTime={recordedAt.toISOString()}>{displayDate.format(recordedAt)}</time></div>
    </ContextActions>
    {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
  </>;
}

function RecordedProfileRow({ record }: { record: RecordedProfile }) {
  const model = record.reportedModel ?? record.requestedModel;
  const disclosure = useRef<HTMLDetailsElement>(null);

  function showQualification() {
    const details = disclosure.current;
    if (!details) return;
    details.open = true;
    const view = details.ownerDocument.defaultView;
    // Focus the disclosure after the menu finishes restoring focus.
    view?.setTimeout(() => view.requestAnimationFrame(() => {
      if (details.isConnected && details.open) details.querySelector("summary")?.focus();
    }));
  }

  return <MatrixTableRow
    cli={<RecordedCli record={record} showQualification={showQualification} />}
    model={<>{model ?? "Provider default"}
      {!record.reportedModel && <div className="text-muted-foreground">Model report unavailable</div>}
      {record.reportedModel && record.requestedModel && record.reportedModel !== record.requestedModel &&
        <div className="text-muted-foreground">Requested · {record.requestedModel}</div>}</>}
    host={record.platform ? windowsVersion(record.platform.host) : "Unavailable"}
    runner={<>{record.runner === "windows-direct" ? "Direct Windows host" : record.runner}
      {record.platform?.guest && <div>{windowsVersion(record.platform.guest)}</div>}
      <div className="text-muted-foreground">{modes[record.mode]}</div></>}
    qualification={<>
      <RecordedCapabilities capabilities={record.capabilities} disclosure={disclosure} />
      {record.validation && <RecordedValidation target={record.validation} />}
    </>} />;
}

function MatrixTableRow({ cli, model, host, runner, qualification }: Omit<MatrixRow, "id">) {
  return <tr className="align-top"><td>{cli}</td><td>{model}</td><td>{host}</td><td>{runner}</td><td>{qualification}</td></tr>;
}

function MatrixTable({ children }: { children: ReactNode }) {
  return <div className="settings-table-scroll" role="region" tabIndex={0} aria-label="Qualification table">
    <table aria-label="Qualification matrix">
      <thead><tr><th scope="col">CLI</th><th scope="col">Model</th><th scope="col">Host</th><th scope="col">Runner</th><th scope="col">Qualification</th></tr></thead>
      <tbody>{children}</tbody>
    </table>
  </div>;
}

function RecordedQualifications({ validationCheckoutId, provider }: { validationCheckoutId: string | null; provider?: Provider }) {
  const [pages, setPages] = useState<(Cursor | null)[]>([null]);
  const cursor = pages.at(-1) ?? null;
  const query = useQuery({
    queryKey: ["desktop-capability-qualifications", cursor, validationCheckoutId, provider],
    queryFn: () => provider
      ? invoke<QualificationPage>("provider_capability_qualifications", { cursor, provider, checkoutId: validationCheckoutId })
      : invoke<QualificationPage>("desktop_capability_qualifications", { cursor, validationCheckoutId }),
    retry: false,
  });
  const next = query.data?.nextCursor;
  return <>
    <p>Historical assessments from submitted dispatches. Results apply to the recorded profile and scope.</p>
    {query.isPending && <p role="status">Loading recorded qualifications…</p>}
    {query.isError && <><p role="alert">Recorded qualifications could not be loaded.</p>
      <Button type="button" variant="outline" onClick={() => void query.refetch()}>Retry</Button></>}
    {query.data?.records.length ? <MatrixTable>{query.data.records.map((record) => <RecordedProfileRow key={record.runId} record={record} />)}</MatrixTable> : query.isSuccess &&
      <p>No submitted dispatch profiles have been recorded.</p>}
    {(pages.length > 1 || next) && <nav className="settings-actions" aria-label="Qualification pages">
      <Button type="button" variant="outline" disabled={pages.length <= 1} onClick={() => setPages((current) => current.slice(0, -1))}>Newer</Button>
      <Button type="button" variant="outline" disabled={!next} onClick={() => next && setPages((current) => [...current, next])}>Older</Button>
    </nav>}
  </>;
}

export function QualificationMatrix({ examples = false, validationCheckoutId = null, provider }: {
  examples?: boolean;
  validationCheckoutId?: string | null;
  provider?: Provider;
}) {
  const [expanded, setExpanded] = useState(false);
  return <details className="settings-matrix" onToggle={(event) => setExpanded(event.currentTarget.open)}>
    <summary>Qualification matrix</summary>
    {examples ? <MatrixTable>{exampleRows.map(({ id, ...row }) => <MatrixTableRow key={id} {...row} />)}</MatrixTable> : expanded &&
      <RecordedQualifications key={validationCheckoutId} validationCheckoutId={validationCheckoutId} provider={provider} />}
  </details>;
}
