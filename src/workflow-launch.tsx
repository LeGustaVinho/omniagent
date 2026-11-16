import { useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { RepositoryMarkdownPicker } from "@/repository-markdown-picker";
import type { WorkspaceProps } from "@/workbench-contract";

type Version = { id: string; revision: string };
type Published = { version: Version; name: string };
type Source = { checkoutId: string; revisionId: string };
type LaunchOrigin = { kind: "editor"; requestId: string }
  | { kind: "agile"; requestId: string; workId: string }
  | { kind: "rule"; ruleId: string; eventId: string; workId: string };
export type WorkflowLaunchReceipt = {
  workflowRunId: string;
  version: Version;
  origin: LaunchOrigin;
};

const versionKey = (version: Version) => JSON.stringify([version.id, version.revision]);

export function preferredWorkflowRevision(draft: { base?: Version | null; publication?: { workflow: Version } | null }) {
  return draft.publication?.workflow ?? draft.base;
}

export function WorkflowLaunchPicker({ projectId, checkoutId, workId, preferred, openWorkspace }: {
  projectId: string;
  checkoutId: string;
  workId?: string;
  preferred?: Version | null;
  openWorkspace: WorkspaceProps["openWorkspace"];
}) {
  const client = useQueryClient();
  const [expanded, setExpanded] = useState(false);
  const [selected, setSelected] = useState("");
  const inFlight = useRef<{ key: string; requestId: string } | null>(null);
  const versions = useQuery({
    queryKey: ["published-workflows-for-launch", projectId],
    queryFn: () => invoke<Published[]>("workflow_authoring_bases", { projectId }),
    enabled: expanded,
  });
  const launch = useMutation({
    mutationFn: ({ workflow, source, requestId }: { workflow: Version; source: Source; requestId: string }) =>
      invoke<{ id: string }>("launch_workflow_manual", { request: { projectId, workflow, source, requestId, workId: workId ?? null } }),
    onSuccess: (run) => {
      inFlight.current = null;
      setExpanded(false);
      void client.invalidateQueries({ queryKey: ["repository-delivery", projectId] });
      openWorkspace("pipelines", run.id);
    },
  });
  const choices = versions.data ?? [];
  const desired = selected || (preferred ? versionKey(preferred) : "");
  const available = choices.find(({ version }) => versionKey(version) === desired) ?? (desired ? undefined : choices[0]);

  function run(revisionId: string) {
    if (!available || launch.isPending) return;
    const source = { checkoutId, revisionId };
    const key = JSON.stringify([projectId, available.version, source, workId]);
    if (inFlight.current?.key !== key) {
      inFlight.current = { key, requestId: crypto.randomUUID() };
    }
    launch.mutate({ workflow: available.version, source, requestId: inFlight.current.requestId });
  }

  return <section className="pipeline-stack" aria-label="Launch workflow">
    <div>
      <Button type="button" variant="outline" aria-expanded={expanded} disabled={launch.isPending}
        onClick={() => setExpanded(!expanded)}>Run published workflow</Button>
    </div>
    {expanded && <div className="pipeline-form pipeline-stack">
      {versions.isPending && <p role="status">Loading published revisions…</p>}
      {versions.data?.length ? <label>Published workflow revision
        <select value={available ? versionKey(available.version) : ""} disabled={launch.isPending}
          onChange={(event) => setSelected(event.currentTarget.value)}>
          {!available && <option value="">Select a published revision</option>}
          {choices.map(({ version, name }) => <option key={versionKey(version)} value={versionKey(version)}>
            {name} · {version.revision.slice(0, 12)}
          </option>)}
        </select>
      </label> : versions.data && <p>Publish a workflow revision to run it here.</p>}
      {available && <RepositoryMarkdownPicker key={checkoutId} checkoutId={checkoutId}
        onDeliver={run} delivering={launch.isPending} launchLabel="Start workflow" />}
      {(versions.error ?? launch.error) && <p role="alert">{String(versions.error ?? launch.error)}</p>}
    </div>}
  </section>;
}
