import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArchiveRestore, FileCode2, HardDrive, RefreshCw, ShieldCheck } from "lucide-react";
import { useEffect, useId, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { DesktopCliPrerequisites } from "@/desktop-cli-prerequisites";
import { ExternalEditorSettings } from "@/external-editor-settings";
import { ripgrepStatusKey, useRipgrepStatus, type RipgrepStatus } from "@/ide-file-search";
import { QualificationMatrix } from "@/qualification-matrix";
import { SystemInstallation } from "@/system-installation";
import { SettingRow } from "@/settings-controls";
import { signedUpdateStatusKey, type SignedUpdateStatus } from "@/workflow-maintenance";

export function SystemSettings() {
  return (
    <div className="settings-system">
      <SystemGroup icon={<HardDrive size={18} />} id="environment" title="Current installation">
        <SystemInstallation />
      </SystemGroup>
      <SystemGroup icon={<ShieldCheck size={18} />} id="qualification" title="Prerequisites and qualification">
        <p>Windows 10 22H2 or Windows 11 x64, WebView2, Git for Windows, ripgrep and a supported provider CLI.</p>
        <DesktopCliPrerequisites />
        <RipgrepSettings />
        <QualificationMatrix />
      </SystemGroup>
      <SystemGroup icon={<FileCode2 size={18} />} id="external-editor" title="External editor">
        <ExternalEditorSettings />
      </SystemGroup>
      <SystemGroup icon={<ArchiveRestore size={18} />} id="backup" title="Backup and restore">
        <p>A backup includes local settings, run history and protected artifacts. Restore checks the original Windows account and OmniAgent installation.</p>
        <ApplicationBackup />
        <ApplicationRestore />
      </SystemGroup>
      <SystemGroup icon={<RefreshCw size={18} />} id="updates" title="Updates and recovery">
        <SignedUpdateHistory />
      </SystemGroup>
    </div>
  );
}

function SignedUpdateHistory() {
  const status = useQuery({
    queryKey: signedUpdateStatusKey, queryFn: () => invoke<SignedUpdateStatus>("signed_update_status"),
  });
  const receipt = status.data?.latest;
  return <>
    {status.isPending && <p role="status">Checking update history…</p>}
    {status.error && <p role="alert" className="settings-result settings-result-error">{String(status.error)}</p>}
    {status.data?.configured === false && <p>This build has no trusted update feed configured.</p>}
    {status.data?.configured && <p>Verify an accepted maintenance package from its workflow delivery to install a signed update.</p>}
    {receipt && <>
      <dl className="settings-facts">
        <dt>Version</dt><dd>{receipt.update.version}</dd>
        <dt>Source revision</dt><dd><code>{receipt.update.sourceRevision}</code></dd>
        <dt>Recovery checkpoint</dt><dd><code>{receipt.backup.checkpointId}</code></dd>
        <dt>Outcome</dt><dd>{receipt.outcome.status.replaceAll("_", " ")}</dd>
      </dl>
      {receipt.outcome.reason && <p role="alert" className="settings-result settings-result-error">{receipt.outcome.reason}</p>}
    </>}
  </>;
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

function SystemGroup({ icon, id, title, children }: { icon: ReactNode; id: string; title: string; children: ReactNode }) {
  const titleId = useId();
  return <section className="settings-system-group" data-setting={id} aria-labelledby={titleId} tabIndex={-1}>
    <h3 id={titleId}>{icon}{title}</h3>
    {children}
  </section>;
}
