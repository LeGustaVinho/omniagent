import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useRef, useState } from "react";
import { Button } from "@/components/ui/button";

type GithubSettings = {
  accountId: number;
  repositoryId: number;
  owner: string;
  repository: string;
  remote: string;
  destinationBranch: string;
  requiredChecks: { context: string; appId: number | null }[];
  allowAutomaticMerge: boolean;
  highRiskPaths?: string[] | null;
};

type CheckRow = { id: number; context: string; appId: string };

const textFields = [
  ["accountId", "GitHub account ID"],
  ["repositoryId", "GitHub repository ID"],
  ["owner", "Repository owner"],
  ["repository", "Repository name"],
  ["remote", "Git remote"],
  ["destinationBranch", "Destination branch"],
] as const;

const fieldClass = "mt-1 block w-full rounded-md border border-input bg-background px-2 py-1.5 text-sm";

function positiveId(value: string): number | undefined {
  const id = Number(value);
  return /^\d+$/.test(value) && Number.isSafeInteger(id) && id > 0 ? id : undefined;
}

export function WorkflowGithubSettings({ projectId }: { projectId: string }) {
  const [open, setOpen] = useState(false);
  const client = useQueryClient();
  const queryKey = ["workflow-github-settings", projectId];
  const policy = useQuery({
    queryKey,
    queryFn: () => invoke<GithubSettings | null>("workflow_github_settings", { projectId }),
    enabled: open,
  });
  const save = useMutation({
    mutationFn: (settings: GithubSettings) => invoke<string>("set_workflow_github_settings", { projectId, settings }),
    onSuccess: (_, settings) => client.setQueryData(queryKey, settings),
  });

  return <details data-setting="workflow-github" className="grid gap-3 pb-5"
    onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary className="cursor-pointer text-base font-semibold">Workflow GitHub settings</summary>
    <div className="mt-3 grid gap-3">
      {open && policy.isPending && !policy.isError && <p role="status" className="text-sm">Loading GitHub settings…</p>}
      {policy.isError && <p role="alert" className="settings-result settings-result-error">
        {String(policy.error)} <button type="button" onClick={() => void policy.refetch()}>Retry</button>
      </p>}
      {policy.data !== undefined && <GithubSettingsForm key={JSON.stringify(policy.data)} settings={policy.data}
        pending={save.isPending} submit={save.mutate} onEdit={save.reset} />}
      {save.isError && <p role="alert" className="settings-result settings-result-error">{String(save.error)}</p>}
      {save.isSuccess && <p role="status" className="text-sm">GitHub settings saved</p>}
    </div>
  </details>;
}

function GithubSettingsForm({ settings, pending, submit, onEdit }: {
  settings: GithubSettings | null;
  pending: boolean;
  submit: (settings: GithubSettings) => void;
  onEdit: () => void;
}) {
  const [identity, setIdentity] = useState({
    accountId: String(settings?.accountId ?? ""),
    repositoryId: String(settings?.repositoryId ?? ""),
    owner: settings?.owner ?? "",
    repository: settings?.repository ?? "",
    remote: settings?.remote ?? "origin",
    destinationBranch: settings?.destinationBranch ?? "",
  });
  const [allowAutomaticMerge, setAllowAutomaticMerge] = useState(settings?.allowAutomaticMerge ?? false);
  const [highRiskPaths, setHighRiskPaths] = useState(settings?.highRiskPaths?.join("\n") ?? "");
  const [checks, setChecks] = useState<CheckRow[]>(() => (settings?.requiredChecks ?? [{ context: "", appId: null }])
    .map(({ context, appId }, id) => ({ id, context, appId: String(appId ?? "") })));
  const nextCheckId = useRef(checks.length);
  const [validationError, setValidationError] = useState("");

  function edited() {
    setValidationError("");
    onEdit();
  }

  function editChecks(rows: CheckRow[]) {
    setChecks(rows);
    edited();
  }

  return <form className="grid gap-3" onSubmit={(event) => {
    event.preventDefault();
    const accountId = positiveId(identity.accountId);
    const repositoryId = positiveId(identity.repositoryId);
    const requiredChecks = checks.map(({ context, appId }) => ({ context, appId: positiveId(appId) ?? null }));
    if (accountId === undefined || repositoryId === undefined
      || checks.some(({ appId }, index) => appId !== "" && requiredChecks[index]?.appId == null)) {
      setValidationError("GitHub IDs must be whole numbers from 1 to 9007199254740991.");
      return;
    }
    if (!checks.length || checks.some(({ context }) => !context.trim())) {
      setValidationError("Add at least one required check and enter a context for every check.");
      return;
    }
    const paths = highRiskPaths.split(/\r?\n/).filter((path) => path.trim());
    submit({
      ...identity,
      accountId,
      repositoryId,
      requiredChecks,
      allowAutomaticMerge,
      highRiskPaths: paths.length ? paths : null,
    });
  }}>
    <fieldset disabled={pending} className="grid gap-3 text-sm">
      <div className="grid gap-3 sm:grid-cols-2">
        {textFields.map(([key, label]) => <label key={key}>{label}
          <input className={fieldClass} required value={identity[key]}
            inputMode={key === "accountId" || key === "repositoryId" ? "numeric" : undefined}
            onChange={(event) => {
              setIdentity({ ...identity, [key]: event.target.value });
              edited();
            }} />
        </label>)}
      </div>
      <fieldset className="grid gap-3 rounded-md border border-border p-3">
        <legend className="px-1 font-medium">Required GitHub checks</legend>
        {checks.map((row, index) => <fieldset key={row.id} className="grid gap-2 rounded-md border border-border p-2">
          <legend className="px-1">Check {index + 1}</legend>
          <div className="grid gap-2 sm:grid-cols-2">
            <label>Check context
              <input className={fieldClass} required value={row.context} onChange={(event) =>
                editChecks(checks.map((check) => check.id === row.id ? { ...check, context: event.target.value } : check))}
              />
            </label>
            <label>GitHub App ID (optional)
              <input className={fieldClass} inputMode="numeric" value={row.appId} onChange={(event) =>
                editChecks(checks.map((check) => check.id === row.id ? { ...check, appId: event.target.value } : check))}
              />
            </label>
          </div>
          <Button type="button" variant="outline" className="justify-self-start"
            aria-label={`Remove check ${index + 1}`}
            onClick={() => editChecks(checks.filter((check) => check.id !== row.id))}>Remove</Button>
        </fieldset>)}
        <Button type="button" variant="outline" className="justify-self-start"
          onClick={() => editChecks([...checks, { id: nextCheckId.current++, context: "", appId: "" }])}>Add check</Button>
      </fieldset>
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={allowAutomaticMerge} onChange={(event) => {
          setAllowAutomaticMerge(event.target.checked);
          edited();
        }} />
        Allow automatic merge when policy permits
      </label>
      <label>High-risk paths, one rule per line
        <textarea className={fieldClass} rows={3} value={highRiskPaths} onChange={(event) => {
          setHighRiskPaths(event.target.value);
          edited();
        }} />
      </label>
      <p className="text-muted-foreground">
        Use a relative file path, a directory ending in /, or a suffix such as *.sql.
        Matching changes require approval. With no rules, every merge requires approval.
      </p>
      {validationError && <p role="alert" className="settings-result settings-result-error">{validationError}</p>}
      <Button type="submit" className="justify-self-start">{pending ? "Saving…" : "Save GitHub settings"}</Button>
    </fieldset>
  </form>;
}
