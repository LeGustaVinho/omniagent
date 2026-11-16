import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Dialog } from "radix-ui";
import { Button } from "@/components/ui/button";
import type { WorkflowGraph } from "@/workflow-graph-model";

type Version = { id: string; revision: string };
export type WorkflowPublication = {
  id: string;
  workflow: Version;
  base: Version | null;
  draftId: string;
  reviewedRevisionId: string;
  originatingRequestId: string | null;
  originatingRunId: string | null;
  decisionSource: "desktop_user";
  confirmedAt: number;
};
type CurrentDefinition = {
  version: Version;
  name: string;
  nodes: { instance: WorkflowGraph["nodes"][number]; revision: string }[];
  wires: WorkflowGraph["wires"];
};
type PublishResult = { status: "published"; publication: WorkflowPublication } | { status: "stale_base"; current: CurrentDefinition };
type Confirmation = { projectId: string; draftId: string; reviewedRevisionId: string; expectedBase: Version | null };

export function WorkflowPublicationControls({ projectId, draftId, revisionId, base, graph, disabled, publication, onPublished }: {
  projectId: string;
  draftId: string;
  revisionId: string;
  base: Version | null;
  graph: WorkflowGraph;
  disabled: boolean;
  publication?: WorkflowPublication | null;
  onPublished: (publication: WorkflowPublication) => void;
}) {
  const [confirmation, setConfirmation] = useState<Confirmation>();
  const publish = useMutation({
    mutationFn: (request: Confirmation) => invoke<PublishResult>("publish_reviewed_workflow", request),
    onSuccess: (result) => {
      setConfirmation(undefined);
      if (result.status === "published") onPublished(result.publication);
    },
  });
  if (publication) {
    return <section aria-label="Publication evidence" className="pipeline-evidence pipeline-stack">
      <p role="status">Published workflow revision <code className="break-all">{publication.workflow.revision}</code></p>
      <dl className="grid gap-1 text-sm">
        <dt>Reviewed draft revision</dt><dd><code className="break-all">{publication.reviewedRevisionId}</code></dd>
        <dt>Publication receipt</dt><dd><code className="break-all">{publication.id}</code></dd>
        <dt>Confirmed locally</dt><dd><time dateTime={new Date(publication.confirmedAt * 1_000).toISOString()}>
          {new Date(publication.confirmedAt * 1_000).toLocaleString()}
        </time></dd>
        {publication.originatingRunId && <><dt>Originating authoring run</dt><dd><code className="break-all">{publication.originatingRunId}</code></dd></>}
      </dl>
    </section>;
  }
  const current = publish.data?.status === "stale_base" ? publish.data.current : undefined;
  return <section aria-label="Publish saved revision" className="pipeline-stack">
    <Dialog.Root open={Boolean(confirmation)} onOpenChange={(value) => { if (!value && !publish.isPending) setConfirmation(undefined); }}>
      <Dialog.Trigger asChild>
        <Button type="button" disabled={disabled || publish.isPending || Boolean(current)} onClick={() => {
          publish.reset();
          setConfirmation({ projectId, draftId, reviewedRevisionId: revisionId, expectedBase: base });
        }}>Review publication</Button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-background/75 backdrop-blur-sm" />
        <Dialog.Content className="fixed left-1/2 top-1/2 z-50 grid w-[min(32rem,calc(100%-2rem))] -translate-x-1/2 -translate-y-1/2 gap-4 rounded-lg border border-border bg-card p-5 shadow-lg">
          <Dialog.Title className="text-lg font-semibold">Publish this pipeline revision</Dialog.Title>
          <Dialog.Description>
            Publish <strong>{graph.name}</strong> to this project. Running it requires a separate action.
          </Dialog.Description>
          <dl className="grid gap-2 text-sm">
            <dt>Reviewed draft revision</dt><dd><code className="break-all">{confirmation?.reviewedRevisionId}</code></dd>
            <dt>Published base</dt><dd>{confirmation?.expectedBase
              ? <code className="break-all">{confirmation.expectedBase.revision}</code> : "New workflow"}</dd>
          </dl>
          {publish.error && <p role="alert">{String(publish.error)}</p>}
          <div className="flex justify-end gap-2">
            <Dialog.Close asChild><Button type="button" variant="outline" disabled={publish.isPending}>Cancel</Button></Dialog.Close>
            <Button type="button" disabled={disabled || publish.isPending || !confirmation}
              onClick={() => { if (confirmation) publish.mutate(confirmation); }}>
              {publish.isPending ? "Publishing…" : "Confirm publication"}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
    {current && <section aria-label="Current published workflow" className="pipeline-evidence pipeline-stack">
      <p role="alert">A newer version was published from a different draft. Your reviewed draft is preserved.</p>
      <p><strong>{current.name}</strong> · <code className="break-all">{current.version.revision}</code></p>
      <details>
        <summary className="cursor-pointer">Compare the current published graph</summary>
        <pre className="max-h-72 overflow-auto whitespace-pre-wrap text-xs">{JSON.stringify({
          name: current.name, nodes: current.nodes.map(({ instance, revision }) => ({ ...instance, revision })), wires: current.wires,
        }, null, 2)}</pre>
      </details>
    </section>}
  </section>;
}
