import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { Button } from "@/components/ui/button";

type RunningBuild = { version: string; sourceCommit: string | null; sourceModified: boolean | null; target: string; profile: string };
type MaintenanceMode = "disabled" | "manual_fixture" | "signed_updater";
type MaintenanceConfiguration = {
  sourceCheckoutId: string;
  workflowId: string;
  build: { executable: string; arguments: string[]; timeoutSeconds: number | null; packagePath: string; versionPath: string };
  requiredChecks: string[];
  activation: MaintenanceMode;
};
type MaintenanceSettings = {
  revision: string;
  configuration: MaintenanceConfiguration;
  buildExecutableRevision: string;
};
type MaintenanceHandoff = {
  id: string;
  workflowRunId: string;
  nodeId: string;
  gitArtifactId: string;
  settingsRevision: string;
  version: string;
  packageName: string;
  sourceRevision: string;
  sourceTree: string;
  sha256: string;
  candidateRevision: string;
  commitEvidenceId: string;
  verificationId: string;
  approvalArtifactId: string;
  approvalSource: "operator" | "automatic_rule";
  reviewArtifactId: string;
  acceptanceArtifactId: string;
  checks: string[];
  activation: { id: string; recordedAt: string; workflowArtifactId: string } | null;
  signedActivation?: { id: string; version: string; sourceRevision: string; sha256: string; workflowArtifactId: string; recordedAt: string } | null;
};
type Maintenance = { runningBuild: RunningBuild; settings: MaintenanceSettings | null; handoffs: MaintenanceHandoff[] };
type WorkflowChoice = { version: { id: string }; name: string };

type SignedUpdatePreview = {
  handoffId: string;
  currentVersion: string;
  version: string;
  sourceRevision: string;
  sha256: string;
  checks: string[];
  verificationId: string;
  approvalArtifactId: string;
};
type SignedUpdateReceipt = {
  id: string;
  update: SignedUpdatePreview;
  backup: { checkpointId: string; root: string };
  outcome: { status: string; reason?: string };
};
export type SignedUpdateStatus = { configured: boolean; latest: SignedUpdateReceipt | null };
export const signedUpdateStatusKey = ["signed-update-status"];

const maintenanceKey = (projectId: string) => ["workflow-maintenance", projectId];
const readMaintenance = (projectId: string) => invoke<Maintenance>("workflow_maintenance", { projectId });
const lines = (source: string) => source.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);

function BuildDescription({ build }: { build: RunningBuild }) {
  return <div>
    <p>Running build <strong>{build.version}</strong> · {build.target} · {build.profile}</p>
    <p>Source commit <code className="break-all">{build.sourceCommit ?? "Unavailable"}</code>
      {build.sourceModified && " · Built from a modified checkout"}</p>
  </div>;
}

export function WorkflowMaintenanceSettings({ checkout }: { checkout: ProjectCheckout }) {
  const [open, setOpen] = useState(false);
  return <details className="pipeline-evidence" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Maintenance workflow</summary>
    {open && <MaintenanceSettingsForm checkout={checkout} />}
  </details>;
}

function MaintenanceSettingsForm({ checkout }: { checkout: ProjectCheckout }) {
  const { projectId } = checkout;
  const maintenance = useQuery({ queryKey: maintenanceKey(projectId), queryFn: () => readMaintenance(projectId) });
  const workflows = useQuery({
    queryKey: ["workflow-authoring-bases", projectId],
    queryFn: () => invoke<WorkflowChoice[]>("workflow_authoring_bases", { projectId }),
  });
  const checkouts = useQuery({
    queryKey: ["registered-checkouts"],
    queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts"),
  });
  const available = Array.from(new Map([
    ["builtin-task-delivery", "Task delivery"],
    ...(workflows.data ?? []).map(({ version, name }): [string, string] => [version.id, name]),
  ]));
  const error = maintenance.error ?? workflows.error ?? checkouts.error;
  return <section className="pipeline-stack pt-3" aria-label="Maintenance workflow settings">
    {error && <p role="alert">{String(error)}</p>}
    {!error && !maintenance.data && <p role="status">Loading maintenance settings…</p>}
    {maintenance.data && <>
      <BuildDescription build={maintenance.data.runningBuild} />
      <MaintenanceConfigurationForm key={maintenance.data.settings?.revision ?? "initial"} projectId={projectId}
        checkoutId={checkout.checkoutId} settings={maintenance.data.settings} workflows={available}
        checkouts={(checkouts.data ?? []).filter((row) => row.projectId === projectId)} />
    </>}
  </section>;
}

function MaintenanceConfigurationForm({ projectId, checkoutId, settings, checkouts, workflows }: {
  projectId: string; checkoutId: string; settings: MaintenanceSettings | null;
  checkouts: ProjectCheckout[]; workflows: [string, string][];
}) {
  const client = useQueryClient();
  const [source, setSource] = useState(settings?.configuration.sourceCheckoutId ?? checkoutId);
  const [workflow, setWorkflow] = useState(settings?.configuration.workflowId ?? "");
  const [build, setBuild] = useState(settings?.configuration.build.executable ?? "");
  const [argumentsText, setArguments] = useState(settings?.configuration.build.arguments.join("\n") ?? "");
  const [timeout, setTimeoutSeconds] = useState(String(settings?.configuration.build.timeoutSeconds ?? ""));
  const [packagePath, setPackagePath] = useState(settings?.configuration.build.packagePath ?? "");
  const [versionPath, setVersionPath] = useState(settings?.configuration.build.versionPath ?? "");
  const [checks, setChecks] = useState(settings?.configuration.requiredChecks.join("\n") ?? "");
  const [mode, setMode] = useState<MaintenanceMode>(settings?.configuration.activation ?? "disabled");
  const timeoutSeconds = timeout.trim() ? Number(timeout) : null;
  const argumentsList = argumentsText ? argumentsText.split(/\r?\n/) : [];
  const canSave = checkouts.some((entry) => entry.checkoutId === source) && workflows.some(([id]) => id === workflow)
    && !!build.trim() && !!packagePath.trim() && !!versionPath.trim() && !!lines(checks).length
    && (timeoutSeconds === null || Number.isSafeInteger(timeoutSeconds) && timeoutSeconds > 0);
  const save = useMutation({
    mutationFn: (configuration: MaintenanceConfiguration) => invoke<MaintenanceSettings>("set_workflow_maintenance", {
      projectId, expectedRevision: settings?.revision ?? null, configuration,
    }),
    onSuccess: () => client.invalidateQueries({ queryKey: maintenanceKey(projectId) }),
  });
  return <form className="pipeline-stack" onSubmit={(event) => {
    event.preventDefault();
    if (!canSave) return;
    save.mutate({
      sourceCheckoutId: source, workflowId: workflow, activation: mode, requiredChecks: lines(checks),
      build: { executable: build.trim(), arguments: argumentsList, timeoutSeconds, packagePath: packagePath.trim(), versionPath: versionPath.trim() },
    });
  }}>
    <fieldset disabled={save.isPending} className="pipeline-stack">
      <label>Registered source checkout <select value={source} onChange={(event) => setSource(event.currentTarget.value)}>
        <option value="">Select checkout</option>
        {checkouts.map((entry) => <option key={entry.checkoutId} value={entry.checkoutId}>{entry.checkoutRoot}</option>)}
      </select></label>
      <label>Delivery workflow <select value={workflow} onChange={(event) => setWorkflow(event.currentTarget.value)}>
        <option value="">Select published workflow</option>
        {workflows.map(([id, name]) => <option key={id} value={id}>{name}</option>)}
      </select></label>
      <label>Build executable <input value={build} onChange={(event) => setBuild(event.currentTarget.value)} /></label>
      <label>Build arguments, one per line <textarea value={argumentsText} onChange={(event) => setArguments(event.currentTarget.value)} /></label>
      <label>Build timeout in seconds <input type="number" min={1} value={timeout} onChange={(event) => setTimeoutSeconds(event.currentTarget.value)} /></label>
      <label>Package path relative to source <input value={packagePath} onChange={(event) => setPackagePath(event.currentTarget.value)} /></label>
      <label>Version output path (relative UTF-8 file) <input value={versionPath} onChange={(event) => setVersionPath(event.currentTarget.value)} /></label>
      <label>Required check names, one per line <textarea value={checks} onChange={(event) => setChecks(event.currentTarget.value)} /></label>
      <label>Activation after handoff <select value={mode} onChange={(event) => setMode(event.currentTarget.value as MaintenanceMode)}>
        <option value="disabled">Disabled</option>
        <option value="manual_fixture">Record a user-confirmed fixture activation</option>
        <option value="signed_updater">Verify and install a signed update with confirmation</option>
      </select></label>
      <Button type="submit" disabled={!canSave || save.isPending}>Save maintenance workflow</Button>
    </fieldset>
    {save.error && <p role="alert">{String(save.error)}</p>}
    {settings && <p>Saved build executable revision <code className="break-all">{settings.buildExecutableRevision}</code></p>}
  </form>;
}

export function WorkflowMaintenanceDelivery(props: {
  projectId: string; workflowRunId: string; workflowId: string; nodeId: string; gitArtifactId: string;
  taskId: string; commitEvidenceId: string;
}) {
  const [open, setOpen] = useState(false);
  return <details className="pipeline-evidence" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Maintenance package</summary>
    {open && <MaintenanceDelivery {...props} />}
  </details>;
}

function MaintenanceDelivery({ projectId, workflowRunId, workflowId, nodeId, gitArtifactId, taskId, commitEvidenceId }: Parameters<typeof WorkflowMaintenanceDelivery>[0]) {
  const client = useQueryClient();
  const maintenance = useQuery({ queryKey: maintenanceKey(projectId), queryFn: () => readMaintenance(projectId) });
  const settings = maintenance.data?.settings;
  const configured = settings?.configuration.workflowId === workflowId;
  const commit = useQuery({
    queryKey: ["verified-delivery-commit", projectId, taskId, commitEvidenceId],
    queryFn: () => invoke<{ sha: string }>("verified_delivery_commit", { projectId, taskId, evidenceId: commitEvidenceId }),
    enabled: configured,
  });
  const refresh = () => client.invalidateQueries({ queryKey: maintenanceKey(projectId) });
  const make = useMutation({
    mutationFn: (expectedSettingsRevision: string) => invoke<MaintenanceHandoff>("create_maintenance_handoff", {
      request: { projectId, workflowRunId, nodeId, gitArtifactId, taskId, commitEvidenceId, expectedSettingsRevision },
    }),
    onSuccess: refresh,
  });
  const activate = useMutation({
    mutationFn: ({ id, settingsRevision, sha256 }: MaintenanceHandoff) => invoke<MaintenanceHandoff>("activate_maintenance_fixture", {
      request: { projectId, handoffId: id, expectedSettingsRevision: settingsRevision, expectedSha256: sha256 },
    }),
    onSuccess: async () => {
      await Promise.all([refresh(), client.invalidateQueries({ queryKey: ["repository-delivery", projectId] })]);
    },
  });
  const handoffs = maintenance.data?.handoffs.filter((item) => item.workflowRunId === workflowRunId &&
    item.nodeId === nodeId && item.gitArtifactId === gitArtifactId && item.commitEvidenceId === commitEvidenceId) ?? [];
  const current = handoffs.find((item) => settings?.revision === item.settingsRevision);
  const error = maintenance.error ?? commit.error ?? make.error ?? activate.error;
  return <section className="pipeline-stack pt-3" aria-label="Maintenance package delivery">
    {error && <p role="alert">{String(error)}</p>}
    {!maintenance.data && !error && <p role="status">Loading accepted package settings…</p>}
    {maintenance.data && <BuildDescription build={maintenance.data.runningBuild} />}
    {maintenance.data && !configured && <p>This delivery has no maintenance build configured in System settings.</p>}
    {configured && commit.data && <p>Accepted source revision <code className="break-all">{commit.data.sha}</code></p>}
    {configured && settings && !current && !handoffs.some((item) => item.activation || item.signedActivation) && <Button type="button" disabled={make.isPending || !commit.data}
      onClick={() => make.mutate(settings.revision)}>{make.isPending ? "Building package…" : "Build accepted package"}</Button>}
    {handoffs.map((item) => <section key={item.id} className="pipeline-evidence pipeline-stack" aria-label={`Candidate handoff ${item.version}`}>
      <p>Candidate build <strong>{item.version}</strong> · {item.packageName}</p>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-2 break-all">
        <dt>Accepted source</dt><dd><code>{item.sourceRevision}</code></dd>
        <dt>Candidate tree</dt><dd><code>{item.sourceTree}</code></dd>
        <dt>Candidate revision</dt><dd><code>{item.candidateRevision}</code></dd>
        <dt>Package SHA-256</dt><dd><code>{item.sha256}</code></dd>
        <dt>Verification</dt><dd>{item.checks.join(", ")} · <code>{item.verificationId}</code></dd>
        <dt>Implementation approval</dt><dd>{item.approvalSource.replaceAll("_", " ")} · <code>{item.approvalArtifactId}</code></dd>
        <dt>Candidate acceptance</dt><dd><code>{item.acceptanceArtifactId}</code></dd>
        <dt>Review</dt><dd><code>{item.reviewArtifactId}</code></dd>
      </dl>
      {item.activation && <p role="status">Fixture activation recorded <code>{item.activation.id}</code>
        {" · "}Workflow output <code className="break-all">{item.activation.workflowArtifactId}</code></p>}
      {item.signedActivation && <p role="status">Signed update {item.signedActivation.version} verified at startup.
        {" "}Workflow output <code className="break-all">{item.signedActivation.workflowArtifactId}</code></p>}
      {configured && item === current && !item.activation && settings?.configuration.activation === "manual_fixture" &&
        <Button type="button" variant="outline" disabled={activate.isPending} onClick={() => activate.mutate(item)}>
          Record fixture activation
        </Button>}
      {configured && item === current && !item.activation && !item.signedActivation && settings?.configuration.activation === "signed_updater" &&
        <SignedHandoffUpdate key={item.id} projectId={projectId} handoff={item} />}
    </section>)}
  </section>;
}

function SignedHandoffUpdate({ projectId, handoff }: { projectId: string; handoff: MaintenanceHandoff }) {
  const client = useQueryClient();
  const request = {
    projectId, handoffId: handoff.id, expectedSettingsRevision: handoff.settingsRevision, expectedSha256: handoff.sha256,
  };
  const preview = useMutation({
    mutationFn: () => invoke<SignedUpdatePreview>("preview_signed_update", { request }),
  });
  const install = useMutation({
    mutationFn: () => invoke<SignedUpdateReceipt>("activate_signed_update", { request }),
    onSettled: () => client.invalidateQueries({ queryKey: signedUpdateStatusKey }),
  });
  const error = preview.error ?? install.error ?? install.data?.outcome.reason;
  return <>
    <Button type="button" variant="outline" disabled={preview.isPending || install.isPending} onClick={() => {
      install.reset();
      preview.mutate();
    }}>
      {preview.isPending ? "Verifying signed package…" : "Verify signed update"}
    </Button>
    {preview.data && <>
      <p>Signature verified for {preview.data.version}. The installed build is {preview.data.currentVersion}.</p>
      <p>OmniAgent will wait for active work to finish, create a recovery checkpoint, and restart to apply the update.</p>
      <Button type="button" disabled={install.isPending} onClick={() => install.mutate()}>
        {install.isPending ? "Preparing installation…" : "Install verified update"}
      </Button>
    </>}
    {error && <p role="alert">{String(error)}</p>}
  </>;
}
