import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { WorkflowExchangeBindings, type ExchangeBinding, type ExchangeBindingRequirement } from "@/workflow-exchange-bindings";
import type { WorkflowAssistanceVersion as Version } from "@/workflow-editor-assistance";
import type { WorkspaceProps } from "@/workbench-contract";

type CatalogArtifact = { version: Version; name: string };
type Conflict = { member: string; node: string | null; port: string | null; message: string; blocking: boolean };
type Preview = {
  name: string;
  members: Record<string, Version>;
  bindings: Record<string, ExchangeBindingRequirement>;
  missingBindings: string[];
  destinationNotes: string[];
  permissions: string[];
  conflicts: Conflict[];
  canImport: boolean;
  reviewDigest: string;
};
type Request = { document: string; bindings: Record<string, ExchangeBinding> };

const permissions: Record<string, string> = {
  repository_writes_require_release: "Repository writes require a workflow release",
  changes_local_git_state: "The workflow can change local Git state",
  implementation_release_uses_destination_policy: "Implementation releases use the destination policy",
  external_mcp_requires_local_approval: "External MCP effects require local approval",
  model_requires_local_authentication: "Model access uses destination profiles",
  agent_can_change_task_records: "The workflow can change task records when authorized",
};
const versionKey = (version: Version) => JSON.stringify(version);
const readable = (key: string) => key.replaceAll("_", " ");

export function WorkflowExchangePanel({ projectId, openWorkspace }: {
  projectId: string; openWorkspace: WorkspaceProps["openWorkspace"];
}) {
  const [open, setOpen] = useState(false);
  return <details className="pipeline-evidence" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Import or export workflow packages</summary>
    {open && <div className="pipeline-stack pt-2">
      <ImportPackage projectId={projectId} openWorkspace={openWorkspace} />
      <ExportPackage projectId={projectId} />
    </div>}
  </details>;
}

function ExportPackage({ projectId }: { projectId: string }) {
  const [open, setOpen] = useState(false);
  const [selection, setSelection] = useState<Set<string>>(new Set());
  const catalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId],
    queryFn: () => invoke<CatalogArtifact[]>("workflow_package_catalog", { projectId }),
    enabled: open,
  });
  const selected = (catalog.data ?? []).filter(({ version }) => selection.has(versionKey(version))).map(({ version }) => version);
  return <details className="pipeline-evidence" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Export published revisions</summary>
    {open && <div className="pipeline-stack pt-2">
      <p>Referenced versions are included in the package.</p>
      {catalog.isPending && <p role="status">Loading published revisions…</p>}
      {catalog.data?.length === 0 && <p>Publish an artifact to export its revision.</p>}
      <fieldset className="grid max-h-52 gap-1 overflow-auto">
        <legend className="sr-only">Published revisions</legend>
        {catalog.data?.map(({ version, name }) => {
          const key = versionKey(version);
          const checked = selection.has(key);
          return <label className="flex items-baseline gap-2" key={key}>
            <input type="checkbox" checked={checked} disabled={!checked && selection.size >= 32} onChange={() => setSelection((current) => {
              const next = new Set(current);
              if (next.has(key)) next.delete(key);
              else next.add(key);
              return next;
            })} />
            <span>{name} · {readable(version.kind)} · {version.destination} · <code>{version.revision.slice(0, 10)}</code></span>
          </label>;
        })}
      </fieldset>
      <WorkflowExchangeDownload projectId={projectId} versions={selected} />
      {catalog.error && <p role="alert">{String(catalog.error)}</p>}
    </div>}
  </details>;
}

export function WorkflowExchangeDownload({ projectId, versions }: { projectId: string; versions: Version[] }) {
  const save = useMutation({
    mutationFn: (selected: Version[]) => invoke<unknown>("export_workflow_exchange", { projectId, selected }),
    onSuccess: (document) => {
      const url = URL.createObjectURL(new Blob([JSON.stringify(document)], { type: "application/json" }));
      const link = window.document.createElement("a");
      link.href = url;
      link.download = "omniagent-workflow-package.json";
      link.click();
      const revoke = URL.revokeObjectURL.bind(URL);
      setTimeout(() => revoke(url), 1_000);
    },
  });
  return <>
    <Button type="button" variant="outline" disabled={!versions.length || save.isPending}
      onClick={() => save.mutate(versions)}>{save.isPending ? "Preparing export…" : "Download package"}</Button>
    {save.error && <p role="alert">{String(save.error)}</p>}
  </>;
}

function ImportPackage({ projectId, openWorkspace }: {
  projectId: string; openWorkspace: WorkspaceProps["openWorkspace"];
}) {
  const client = useQueryClient();
  const [document, setDocument] = useState("");
  const [bindings, setBindings] = useState<Record<string, ExchangeBinding>>({});
  const [review, setReview] = useState<{ request: Request; preview: Preview }>();
  const file = useMutation({
    mutationFn: async (source: File) => {
      if (source.size > 512 * 1024) throw new Error("The workflow package exceeds 512 KiB.");
      return source.text();
    },
    onMutate: () => { setDocument(""); setBindings({}); setReview(undefined); },
    onSuccess: setDocument,
  });
  const preview = useMutation({
    mutationFn: (request: Request) => invoke<Preview>("preview_workflow_exchange", { projectId, ...request }),
    onSuccess: (result, request) => setReview({ request, preview: result }),
  });
  const importDraft = useMutation({
    mutationFn: (reviewed: { request: Request; preview: Preview }) => invoke<{ id: string }>("import_reviewed_workflow_exchange", {
      projectId, ...reviewed.request, reviewedDigest: reviewed.preview.reviewDigest,
    }),
    onSuccess: (draft) => {
      void client.invalidateQueries({ queryKey: ["workflow-packages", projectId] });
      openWorkspace("pipelines", `authoring:package:${draft.id}`);
    },
  });
  const busy = file.isPending || preview.isPending || importDraft.isPending;
  const current = review?.request.document === document && review?.request.bindings === bindings;
  const inspected = review?.preview;

  function readFile(source?: File) {
    preview.reset();
    importDraft.reset();
    if (source) {
      file.mutate(source);
    } else {
      file.reset();
      setDocument("");
      setBindings({});
      setReview(undefined);
    }
  }

  function bind(key: string, binding: ExchangeBinding | undefined) {
    importDraft.reset();
    setBindings((current) => {
      if (JSON.stringify(current[key]) === JSON.stringify(binding)) return current;
      const next = { ...current };
      if (binding) next[key] = binding;
      else delete next[key];
      return next;
    });
  }

  return <section className="pipeline-stack" aria-label="Import workflow package">
    <label className="grid gap-1">Workflow package file
      <input type="file" accept=".json,application/json" disabled={busy}
        onChange={(event) => readFile(event.currentTarget.files?.[0])} />
    </label>
    {inspected && <div className="pipeline-stack">
      <h3 className="font-medium">{inspected.name}</h3>
      {!current && <p role="status">Preview the destination again after changing bindings.</p>}
      <ul aria-label="Imported revisions">
        {Object.entries(inspected.members).map(([key, version]) => <li key={key} className="break-all">
          {readable(version.kind)} · {version.destination} · <code>{version.revision}</code>
        </li>)}
      </ul>
      {!!inspected.permissions.length && <section aria-label="Execution permissions">
        <h4 className="font-medium">Execution permissions</h4>
        <ul>{inspected.permissions.map((permission) => <li key={permission}>{permissions[permission] ?? readable(permission)}</li>)}</ul>
      </section>}
      {!!inspected.conflicts.length && <section aria-label="Import conflicts">
        <h4 className="font-medium">Import conflicts</h4>
        <ul>{inspected.conflicts.map((issue, index) => <li key={index}>
          {[issue.member, issue.node, issue.port].filter(Boolean).join(" · ")} {issue.blocking && "· Blocking"} · {issue.message}
        </li>)}</ul>
      </section>}
      {!!inspected.destinationNotes.length && <ul aria-label="Destination notes">
        {inspected.destinationNotes.map((note) => <li key={note}>{note}</li>)}
      </ul>}
      <WorkflowExchangeBindings projectId={projectId} requirements={inspected.bindings} disabled={busy} onChange={bind} />
      {!!inspected.missingBindings.length && <p>{inspected.missingBindings.length} required destination bindings need review.</p>}
    </div>}
    <div className="flex flex-wrap gap-2">
      <Button type="button" variant="outline" disabled={busy || !document} onClick={() => {
        importDraft.reset();
        preview.mutate({ document, bindings });
      }}>{preview.isPending ? "Reviewing…" : "Preview import"}</Button>
      {review && <Button type="button" disabled={busy || !current || !review.preview.canImport || importDraft.isError || importDraft.isSuccess}
        onClick={() => importDraft.mutate(review)}>{importDraft.isPending ? "Importing…" : "Import as draft"}</Button>}
    </div>
    {(file.error || preview.error || importDraft.error) && <p role="alert">{String(file.error || preview.error || importDraft.error)}</p>}
  </section>;
}
