import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArchiveRestore, HardDrive, RefreshCw, ShieldCheck } from "lucide-react";
import { useEffect, useReducer, useRef, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { DesktopCliPrerequisites } from "@/desktop-cli-prerequisites";
import { ripgrepStatusKey, useRipgrepStatus, type RipgrepStatus } from "@/ide-file-search";
import { IntegrationNotice, SettingRow } from "@/settings-controls";
import type { WorkspaceProps } from "@/workbench-contract";

const previousBuild = "0.1.0";
const candidateBuild = "0.2.0";
const installation = "example-desktop-001";
const approvedRevision = "9b00ad2";
const candidateDigest = "sha256:bc60ba7bd257ce3b08f7be0ff7f98b91331fa34f599d257e9e79e451c0f1181a";

const steps = {
  backup: ["Snapshot SQLite", "Collect protected evidence", "Verify the manifest"],
  restore: ["Stage archive", "Verify the original profile", "Restore recovered records"],
  install: ["Verify signature and approval", "Drain queue", "Save recovery checkpoint", "Run Windows installer", "Verify the new build"],
  rollback: ["Select previous signed build", "Restore matching checkpoint", "Verify restored build"],
} as const;

type OperationKind = keyof typeof steps;
type Archive = "recent" | "older" | "corrupt" | "different-profile" | `backup-${number}`;
type UpdateOutcome = "success" | "launch-failed" | "signature-failed";
type Status = { type: "error" | "success"; message: string };
type Operation = { kind: OperationKind; index: number };

export type MaintenanceState = {
  installed: string;
  archive: Archive;
  preview: boolean;
  outcome: UpdateOutcome;
  operation: Operation | null;
  backups: number;
  activateApproved: boolean;
  recoveryRequired: boolean;
  status: Status | null;
};

type MaintenanceAction =
  | { type: "archive"; archive: Archive }
  | { type: "outcome"; outcome: UpdateOutcome }
  | { type: "preview" }
  | { type: "activation-policy"; enabled: boolean }
  | { type: "start"; kind: OperationKind }
  | { type: "advance" };

function invalidArchive(archive: Archive) {
  if (archive === "corrupt") return "Manifest integrity check failed. Choose an intact backup.";
  if (archive === "different-profile") return "This archive belongs to another Windows user profile.";
  return null;
}

const initialMaintenance: MaintenanceState = {
  installed: previousBuild,
  archive: "recent",
  preview: false,
  outcome: "success",
  operation: null,
  backups: 0,
  activateApproved: false,
  recoveryRequired: false,
  status: null,
};

function simulateMaintenance(state: MaintenanceState, action: MaintenanceAction): MaintenanceState {
  switch (action.type) {
    case "archive":
      return { ...state, archive: action.archive, preview: false, status: null };
    case "outcome":
      return { ...state, outcome: action.outcome, status: null };
    case "preview":
      return { ...state, preview: true, status: null };
    case "activation-policy":
      return { ...state, activateApproved: action.enabled };
    case "start":
      if (state.operation || (action.kind === "restore" && (!state.preview || invalidArchive(state.archive)))) {
        return state;
      }
      if (action.kind === "install" && state.outcome === "signature-failed") {
        return {
          ...state,
          status: { type: "error", message: "The package signature did not match. The current build is available." },
        };
      }
      return { ...state, operation: { kind: action.kind, index: 0 }, status: null };
    case "advance": {
      const operation = state.operation;
      if (!operation) return state;
      if (operation.index + 1 < steps[operation.kind].length) {
        return { ...state, operation: { ...operation, index: operation.index + 1 } };
      }
      if (operation.kind === "install" && state.outcome === "launch-failed") {
        return {
          ...state,
          operation: null,
          recoveryRequired: true,
          status: { type: "error", message: "The candidate failed its restart check. The saved checkpoint can be recovered." },
        };
      }
      const message = {
        backup: "Backup manifest and protected records verified.",
        restore: "Restored historical runs, drafts and evidence.",
        install: "The new build passed its startup checks. Queue admission resumed.",
        rollback: "Restored the previous build and its matching checkpoint.",
      }[operation.kind];
      return {
        ...state,
        operation: null,
        preview: operation.kind === "restore" ? false : state.preview,
        installed: operation.kind === "install" ? candidateBuild : operation.kind === "rollback" ? previousBuild : state.installed,
        recoveryRequired: false,
        backups: state.backups + (operation.kind === "backup" ? 1 : 0),
        status: { type: "success", message },
      };
    }
  }
}

export function useMaintenanceSimulation() {
  const [state, dispatch] = useReducer(simulateMaintenance, initialMaintenance);
  return { state, dispatch };
}

type Simulation = ReturnType<typeof useMaintenanceSimulation>;

export function SystemSettings({
  simulated,
  startSimulation,
}: {
  simulated?: Simulation;
  startSimulation?: WorkspaceProps["startSimulation"];
}) {
  const runtime = useQuery({
    queryKey: ["runtime-status"],
    queryFn: () => invoke<string>("runtime_status"),
    enabled: !simulated,
  });
  const build = useQuery({
    queryKey: ["application-version"], queryFn: getVersion, enabled: !simulated, staleTime: Infinity,
  });

  if (simulated) return <SimulatedSystem {...simulated} />;
  return (
    <div className="settings-system">
      <SystemGroup icon={<HardDrive size={18} />} id="environment" title="Current installation">
        <dl className="settings-facts">
          <dt>Build</dt><dd>{build.data ?? (build.isError ? "Unavailable" : "Loading…")}</dd>
          <dt>Runtime</dt><dd>{runtime.data ?? (runtime.isError ? "Unavailable" : "Checking…")}</dd>
          <dt>Data directory</dt><dd>Local Windows application data</dd>
        </dl>
      </SystemGroup>
      <IntegrationNotice start={startSimulation} scenario="windows">
        Try the example installation and update recovery flows.
      </IntegrationNotice>
      <SystemGroup icon={<ShieldCheck size={18} />} id="qualification" title="Prerequisites and qualification">
        <p>Windows 10 22H2 or Windows 11 x64, WebView2, Git for Windows, ripgrep and a supported provider CLI.</p>
        <DesktopCliPrerequisites />
        <RipgrepSettings />
        <QualificationMatrix />
      </SystemGroup>
      <SystemGroup icon={<ArchiveRestore size={18} />} id="backup" title="Backup and restore">
        <p>A backup includes local settings, run history and protected artifacts. Restore checks the original Windows account and OmniAgent installation.</p>
        <ApplicationBackup />
        <ApplicationRestore />
      </SystemGroup>
      <SystemGroup icon={<RefreshCw size={18} />} id="updates" title="Updates and recovery">
        <p>Review the candidate version, Git revision, signature and approvals before applying an update.</p>
        <div className="settings-actions">
          <Button variant="outline" disabled>Check for updates</Button>
          <Button variant="outline" disabled>Install signed build</Button>
        </div>
        <dl className="settings-facts">
          <dt>Candidate</dt><dd>Awaiting update inventory</dd>
          <dt>Signature and approval</dt><dd>Unavailable</dd>
        </dl>
      </SystemGroup>
    </div>
  );
}

function ApplicationBackup() {
  const backup = useMutation({
    mutationFn: () => invoke<{ root: string } | null>("create_application_backup"),
  });
  return <>
    <Button type="button" variant="outline" disabled={backup.isPending} onClick={() => backup.mutate()}>
      {backup.isPending ? "Creating backup…" : "Create backup"}
    </Button>
    {backup.data && <p role="status" className="settings-result">Verified backup saved to <code>{backup.data.root}</code></p>}
    {backup.error && <p role="alert" className="settings-result settings-result-error">{String(backup.error)}</p>}
  </>;
}

type RestoreReceipt = { checkpointId: string; schemaBefore: number; schemaAfter: number };
type RestoreStatus = { active: RestoreReceipt | null; pending: RestoreReceipt | null; rejection: string | null };
type SelectedBackup = {
  archive: string;
  preview: { checkpointId: string; schemaVersion: number; manifestSha256: string; packageCount: number };
};

function ApplicationRestore() {
  const client = useQueryClient();
  const [selection, setSelection] = useState<SelectedBackup>();
  const statusKey = ["application-restore-status"];
  const status = useQuery({
    queryKey: statusKey,
    queryFn: () => invoke<RestoreStatus>("application_restore_status"),
  });
  const preview = useMutation({
    mutationFn: () => invoke<SelectedBackup | null>("preview_application_restore"),
    onSuccess: (selected) => {
      if (selected) setSelection(selected);
    },
  });
  const stage = useMutation({
    mutationFn: (selected: SelectedBackup) => invoke<RestoreReceipt>("stage_application_restore", {
      archive: selected.archive,
      manifestSha256: selected.preview.manifestSha256,
    }),
    onSuccess: (receipt) => {
      setSelection(undefined);
      client.setQueryData<RestoreStatus>(statusKey, (old) => ({
        active: old?.active ?? null,
        pending: receipt,
        rejection: null,
      }));
      void client.invalidateQueries({ queryKey: statusKey });
    },
  });
  const pending = status.data?.pending;
  const active = status.data?.active;
  const busy = preview.isPending || stage.isPending;
  const error = preview.error ?? stage.error ?? status.error;
  return <>
    <Button type="button" variant="outline" disabled={busy || Boolean(pending)} onClick={() => {
      stage.reset();
      setSelection(undefined);
      preview.mutate();
    }}>
      {preview.isPending ? "Verifying backup…" : "Preview restore"}
    </Button>
    {selection && !pending && <>
      <dl className="settings-facts">
        <dt>Archive</dt><dd><code>{selection.archive}</code></dd>
        <dt>Verified checkpoint</dt><dd><code>{selection.preview.checkpointId}</code></dd>
        <dt>Database schema</dt><dd>{selection.preview.schemaVersion}</dd>
        <dt>Maintenance packages</dt><dd>{selection.preview.packageCount}</dd>
      </dl>
      <p>Restoring this checkpoint replaces the settings, history and evidence available in the app after restart.</p>
      <Button type="button" disabled={busy} onClick={() => stage.mutate(selection)}>
        {stage.isPending ? "Staging restore…" : "Stage restore for next restart"}
      </Button>
    </>}
    {pending && <p role="status" className="settings-result">Restore staged. Quit OmniAgent from the tray and reopen it to apply checkpoint <code>{pending.checkpointId}</code>.</p>}
    {active && <p className="settings-result">Restored checkpoint <code>{active.checkpointId}</code>.</p>}
    {status.data?.rejection && <p role="alert" className="settings-result settings-result-error">{status.data.rejection}</p>}
    {error && <p role="alert" className="settings-result settings-result-error">{String(error)}</p>}
  </>;
}

function RipgrepSettings() {
  const client = useQueryClient();
  const status = useRipgrepStatus();
  const [draft, setDraft] = useState<string>();
  const current = status.data?.configuredExecutable ?? "";
  const input = draft ?? current;
  const path = input.trim().replace(/^"(.*)"$/, "$1");
  const save = useMutation({
    mutationFn: (executable: string | null) => invoke<RipgrepStatus>("set_ripgrep_executable", { executable }),
    onSuccess: (next) => {
      setDraft(undefined);
      client.setQueryData(ripgrepStatusKey, next);
    },
  });

  return <form aria-label="ripgrep settings" onSubmit={(event) => {
    event.preventDefault();
    if (path !== current) save.mutate(path || null);
  }}>
    <dl className="settings-facts">
      <dt>ripgrep</dt>
      <dd>{status.isPending ? "Checking…" : status.data?.version ?? "Unavailable"}</dd>
      {status.data?.executable && <><dt>Detected executable</dt><dd><code>{status.data.executable}</code></dd></>}
    </dl>
    <SettingRow id="ripgrep-executable" label="ripgrep executable" description="Enter the absolute path to rg.exe. Leave it blank to use PATH.">
      <input id="ripgrep-executable" type="text" value={input} placeholder="Automatic (PATH)" disabled={status.isPending || save.isPending}
        onChange={(event) => { save.reset(); setDraft(event.target.value); }} />
    </SettingRow>
    <div className="settings-actions">
      <Button type="submit" variant="outline" disabled={status.isPending || save.isPending || path === current}>Save ripgrep path</Button>
    </div>
    {(status.error || status.data?.issue || save.error) && <p className="settings-result settings-result-error" role="alert">
      {String(save.error ?? status.error ?? status.data?.issue)}
    </p>}
  </form>;
}

function SimulatedSystem({ state, dispatch }: Simulation) {
  const [updateAvailable, setUpdateAvailable] = useState(false);
  const progress = useRef<HTMLElement>(null);
  const [activateApproved, setActivateApproved] = useState(state.activateApproved);
  const operation = state.operation;
  const active = operation !== null;
  const archiveError = invalidArchive(state.archive);
  useEffect(() => {
    if (active) progress.current?.scrollIntoView?.({ block: "nearest" });
  }, [active]);

  return (
    <div className="settings-system">
      <SystemGroup icon={<HardDrive size={18} />} id="environment" title="Current installation">
        <dl className="settings-facts">
          <dt>Running build</dt><dd>{state.installed}</dd>
          <dt>Installation ID</dt><dd>{installation}</dd>
          <dt>Data directory</dt><dd><code>%LOCALAPPDATA%\OmniAgent</code></dd>
          <dt>Queue admission</dt><dd>{state.recoveryRequired ? "Awaiting recovery" : active && state.operation?.kind !== "backup" ? "Paused" : "Available"}</dd>
        </dl>
      </SystemGroup>
      {state.status && <p className={`settings-result settings-result-${state.status.type}`}
        role={state.status.type === "error" ? "alert" : "status"}>{state.status.message}</p>}
      {operation && (
        <section ref={progress} className="settings-progress" aria-label="Maintenance progress">
          <div className="settings-progress-header">
            <h3>Maintenance progress</h3>
            <span>{operation.index + 1} / {steps[operation.kind].length}</span>
          </div>
          <progress value={operation.index + 1} max={steps[operation.kind].length}
            aria-label="Maintenance step" />
          <ol>
            {steps[operation.kind].map((step, index) => (
              <li key={step} aria-current={index === operation.index ? "step" : undefined}>
                <span>{index < operation.index ? "Complete" : index === operation.index ? "Current" : "Pending"}</span>
                {step}
              </li>
            ))}
          </ol>
          <Button type="button" onClick={() => dispatch({ type: "advance" })}>
            {operation.index + 1 === steps[operation.kind].length ? "Finish" : "Continue"}
          </Button>
        </section>
      )}
      <SystemGroup icon={<ShieldCheck size={18} />} id="qualification" title="Prerequisites and qualification">
        <dl className="settings-facts">
          <dt>Windows</dt><dd>Windows 11 24H2 x64</dd>
          <dt>WebView2</dt><dd>Evergreen detected</dd>
          <dt>Git for Windows</dt><dd>Available</dd>
          <dt>ripgrep</dt><dd>Available</dd>
        </dl>
        <QualificationMatrix examples />
      </SystemGroup>
      <SystemGroup icon={<ArchiveRestore size={18} />} id="backup" title="Backup and restore">
        <p>Snapshots retain the SQLite database, application settings and protected run evidence.</p>
        <div className="settings-actions">
          <Button variant="outline" type="button" disabled={active || state.recoveryRequired}
            onClick={() => dispatch({ type: "start", kind: "backup" })}>Create backup</Button>
          <span>{state.backups} additional backups</span>
        </div>
        <SettingRow id="backup-source" label="Archive source" description="Restore checks this installation and the original Windows profile.">
          <select id="backup-source" value={state.archive} disabled={active} onChange={(event) => {
            dispatch({ type: "archive", archive: event.target.value as Archive });
          }}>
            <option value="recent">Backup · settings revision 14</option>
            <option value="older">Backup · previous schema</option>
            <option value="corrupt">Example with a damaged manifest</option>
            <option value="different-profile">Example from another Windows user</option>
            {Array.from({ length: state.backups }, (_, index) => (
              <option value={`backup-${index + 1}`} key={index}>Created snapshot #{index + 1}</option>
            ))}
          </select>
        </SettingRow>
        <Button variant="outline" type="button" disabled={active || state.recoveryRequired}
          onClick={() => dispatch({ type: "preview" })}>Preview restore</Button>
        {state.preview && (archiveError ? <p role="alert" className="settings-result settings-result-error">{archiveError}</p> : (
          <>
            <dl className="settings-facts">
              <dt>Manifest</dt><dd>{state.archive === "older" ? "Valid · prior schema, migration required" : "Valid · current schema"}</dd>
              <dt>Installation</dt><dd>Matches {installation}</dd>
              <dt>Protected records</dt><dd>Prompts, responses and saved diffs</dd>
            </dl>
            <Button type="button" disabled={active || state.recoveryRequired}
              onClick={() => dispatch({ type: "start", kind: "restore" })}>Restore selected backup</Button>
          </>
        ))}
      </SystemGroup>
      <SystemGroup icon={<RefreshCw size={18} />} id="updates" title="Updates and recovery">
        <SettingRow id="update-outcome" label="Update example">
          <select id="update-outcome" value={state.outcome} disabled={active || state.recoveryRequired}
            onChange={(event) => {
              dispatch({ type: "outcome", outcome: event.target.value as UpdateOutcome });
            }}>
            <option value="success">Successful install</option>
            <option value="launch-failed">Startup verification fails</option>
            <option value="signature-failed">Package signature rejected</option>
          </select>
        </SettingRow>
        <Button type="button" variant="outline" disabled={active || state.recoveryRequired}
          onClick={() => {
            setUpdateAvailable(true);
            if (state.activateApproved && state.outcome !== "signature-failed" && state.installed !== candidateBuild) {
              dispatch({ type: "start", kind: "install" });
            }
          }}>Check for updates</Button>
        {updateAvailable && (
          <>
            <dl className="settings-facts">
              <dt>Version</dt><dd>{candidateBuild}</dd>
              <dt>Source revision</dt><dd><code>{approvedRevision}</code></dd>
              <dt>Package digest</dt><dd><code>{candidateDigest}</code></dd>
              <dt>Approval</dt><dd>Build, test and candidate review recorded</dd>
              <dt>Signature</dt><dd>{state.outcome === "signature-failed" ? "Rejected" : "Verified for this example"}</dd>
            </dl>
            <Button type="button" disabled={active || state.recoveryRequired || state.installed === candidateBuild}
              onClick={() => dispatch({ type: "start", kind: "install" })}>Install signed build</Button>
          </>
        )}
        {state.recoveryRequired && <Button type="button" disabled={active}
          onClick={() => dispatch({ type: "start", kind: "rollback" })}>Restore previous build</Button>}
        <form onSubmit={(event) => {
          event.preventDefault();
          dispatch({ type: "activation-policy", enabled: activateApproved });
        }}>
          <SettingRow id="activate-approved" label="Automatically activate approved builds"
            description="Apply when the signed package matches its approved revision and digest.">
            <input id="activate-approved" type="checkbox" checked={activateApproved}
              onChange={(event) => setActivateApproved(event.target.checked)} disabled={active} />
          </SettingRow>
          <div className="settings-save">
            <Button type="submit" variant="outline" disabled={active || activateApproved === state.activateApproved}>Save activation policy</Button>
          </div>
        </form>
      </SystemGroup>
    </div>
  );
}

export function QualificationMatrix({ examples = false }: { examples?: boolean }) {
  return (
    <details className="settings-matrix">
      <summary>Qualification matrix</summary>
      <div className="settings-table-scroll">
        <table>
          <thead><tr><th>CLI</th><th>Model</th><th>Host</th><th>Runner</th><th>Qualification</th></tr></thead>
          <tbody>{examples ? <>
            <tr><td>Codex</td><td>GPT-6.1 Sol</td><td>Windows 11</td><td>Direct</td><td>Verified · read-only and writing</td></tr>
            <tr><td>Claude Code</td><td>Claude Sonnet 5.5</td><td>Windows 11</td><td>Direct</td><td>Verified · approval prompts</td></tr>
            <tr><td>Antigravity</td><td>Gemini 4 Argon</td><td>Windows 10 22H2</td><td>Hyper-V guest</td><td>Blocked · pairing required</td></tr>
          </> : <tr><td colSpan={5}>Qualification data unavailable</td></tr>}</tbody>
        </table>
      </div>
    </details>
  );
}

function SystemGroup({ icon, id, title, children }: { icon: ReactNode; id: string; title: string; children: ReactNode }) {
  return <section className="settings-system-group" data-setting={id}>
    <h3>{icon}{title}</h3>
    {children}
  </section>;
}
