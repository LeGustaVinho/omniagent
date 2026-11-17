import { useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { RepositoryMarkdownPicker } from "@/repository-markdown-picker";
import { TypedWorkflowLaunchForm, type TypedLaunchFields, type WorkflowStartSchema } from "@/workflow-launch-fields";
import { workflowRunTarget } from "@/workflow-execution-model";
import type { WorkspaceProps } from "@/workbench-contract";

type Version = { id: string; revision: string };
type Published = { version: Version; name: string; start?: WorkflowStartSchema | null };
type Source = { checkoutId: string; revisionId: string };
type LaunchFields = TypedLaunchFields | { source: Source; workId: string | null };
type LaunchRequest = LaunchFields & { projectId: string; workflow: Version; requestId: string };
type LaunchOrigin = { kind: "editor"; requestId: string }
  | { kind: "agile"; requestId: string; workId: string }
  | { kind: "rule"; ruleId: string; eventId: string; workId: string };
export type WorkflowLaunchReceipt = {
  workflowRunId: string;
  version: Version;
  origin: LaunchOrigin;
  checkoutId?: string;
  source?: { id: string; checkoutId: string; relativePath: string } | null;
  inputs?: Record<string, unknown>;
};

const versionKey = (version: Version) => JSON.stringify([version.id, version.revision]);

export function preferredWorkflowRevision(draft: { base?: Version | null; publication?: { workflow: Version } | null }) {
  return draft.publication?.workflow ?? draft.base;
}

export function WorkflowLaunchPicker({ projectId, checkoutId, pinnedDocumentRevisionId, workId, preferred, openWorkspace }: {
  projectId: string;
  checkoutId: string;
  pinnedDocumentRevisionId?: string;
  workId?: string;
  preferred?: Version | null;
  openWorkspace: WorkspaceProps["openWorkspace"];
}) {
  const client = useQueryClient();
  const [expanded, setExpanded] = useState(false);
  const [selected, setSelected] = useState("");
  const inFlight = useRef<{ key: string; requestId: string } | null>(null);
  const pinned = pinnedDocumentRevisionId !== undefined;
  const showing = pinned || expanded;
  const versions = useQuery({
    queryKey: ["workflow-authoring-bases", projectId],
    queryFn: () => invoke<Published[]>("workflow_authoring_bases", { projectId }),
    enabled: showing,
  });
  const launch = useMutation({
    mutationFn: (request: LaunchRequest) => invoke<{ id: string }>("launch_workflow_manual", { request }),
    onSuccess: (run) => {
      inFlight.current = null;
      setExpanded(false);
      void client.invalidateQueries({ queryKey: ["repository-delivery", projectId] });
      openWorkspace("pipelines", workflowRunTarget(run.id));
    },
  });
  const choices = versions.data ?? [];
  const desired = selected || (preferred ? versionKey(preferred) : "");
  const available = choices.find(({ version }) => versionKey(version) === desired) ?? (desired ? undefined : choices[0]);

  function run(fields: LaunchFields) {
    if (!available || launch.isPending) return;
    const input = { projectId, workflow: available.version, ...fields };
    const key = JSON.stringify(input);
    if (inFlight.current?.key !== key) {
      inFlight.current = { key, requestId: crypto.randomUUID() };
    }
    launch.mutate({ ...input, requestId: inFlight.current.requestId });
  }

  return <section className="pipeline-stack" aria-label="Launch workflow">
    {!pinned && <div>
      <Button type="button" variant="outline" aria-expanded={expanded} disabled={launch.isPending}
        onClick={() => setExpanded(!expanded)}>Run published workflow</Button>
    </div>}
    {showing && <div className="pipeline-form pipeline-stack">
      {versions.isPending && <p role="status">Loading published revisions…</p>}
      {versions.data?.length ? <label>Published workflow revision
        <select value={available ? versionKey(available.version) : ""} disabled={launch.isPending}
          onChange={(event) => { setSelected(event.currentTarget.value); launch.reset(); }}>
          {!available && <option value="">Select a published revision</option>}
          {choices.map(({ version, name }) => <option key={versionKey(version)} value={versionKey(version)}>
            {name} · {version.revision.slice(0, 12)}
          </option>)}
        </select>
      </label> : versions.data && <p>Publish a workflow revision to run it here.</p>}
      {available?.start?.requiresEvent ? <div className="pipeline-stack">
        <p className="pipeline-note">This workflow uses an Agile event from an automation rule.</p>
        <Button type="button" variant="outline" className="justify-self-start"
          onClick={() => openWorkspace("pipelines", "automations")}>Open automation rules</Button>
      </div> : available?.start ? <TypedWorkflowLaunchForm key={versionKey(available.version)} projectId={projectId}
        initialCheckoutId={checkoutId} pinnedDocumentRevisionId={pinnedDocumentRevisionId} workId={workId}
        schema={available.start} pending={launch.isPending} onLaunch={run} />
        : available && <RepositoryMarkdownPicker key={checkoutId} checkoutId={checkoutId}
          pinnedRevisionId={pinnedDocumentRevisionId}
          onDeliver={(revisionId) => run({ source: { checkoutId, revisionId }, workId: workId ?? null })}
          delivering={launch.isPending} launchLabel="Start workflow" />}
      {(versions.error ?? launch.error) && <p role="alert">{String(versions.error ?? launch.error)}</p>}
    </div>}
  </section>;
}
