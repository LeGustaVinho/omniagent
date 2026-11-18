import { useState, type FormEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Dialog } from "radix-ui";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { Button } from "@/components/ui/button";
import { EffortSelect } from "@/components/effort-select";
import { ModelPicker } from "@/components/model-picker";
import { availableModelSelection, sameModelSelection, useLiveModelOptions } from "@/live-models";
import type { WorkspaceProps } from "@/workbench-contract";

export type WorkflowAssistanceVersion = {
  kind: string;
  id: string;
  revision: string;
  destination: "project" | "library";
};

type Artifact = { name: string; version: WorkflowAssistanceVersion };
type AssistanceProps = Pick<WorkspaceProps, "openWorkspace"> & {
  projectId: string;
  checkoutId: string;
  artifact?: Artifact;
  disabled?: boolean;
};

const providers = ["codex", "claude"] as const;

export function WorkflowEditorAssistance({ disabled, ...props }: AssistanceProps) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  return <Dialog.Root open={open} onOpenChange={(next) => { if (!busy) setOpen(next); }}>
    <Dialog.Trigger asChild><Button type="button" variant="outline" disabled={disabled}>Assist with AI</Button></Dialog.Trigger>
    {open && <EditorAssistanceDialog key={props.artifact?.version.revision ?? "new"} {...props}
      onBusy={setBusy} onDone={() => setOpen(false)} />}
  </Dialog.Root>;
}

function EditorAssistanceDialog({ projectId, checkoutId, artifact, openWorkspace, onBusy, onDone }: AssistanceProps & {
  onBusy: (busy: boolean) => void;
  onDone: () => void;
}) {
  const client = useQueryClient();
  const [selected, setSelected] = useState<ModelSelection | null>(null);
  const [effort, setEffort] = useState("");
  const [brief, setBrief] = useState("");
  const models = useLiveModelOptions(providers);
  const selection = availableModelSelection(models.options, selected);
  const model = models.models.find(({ option }) => sameModelSelection(option.selection, selection));
  const request = useMutation({
    mutationFn: () => invoke<{ id: string }>("start_workflow_editor_authoring", {
      projectId, checkoutId, selection, effort: model?.efforts.includes(effort) ? effort : null,
      base: artifact?.version ?? null, brief: brief.trim(),
    }),
    onMutate: () => onBusy(true),
    onError: () => onBusy(false),
    onSuccess: (draft) => {
      onBusy(false);
      void client.invalidateQueries({ queryKey: ["workflow-packages", projectId] });
      onDone();
      openWorkspace("pipelines", `authoring:package:${draft.id}`);
    },
  });
  const ready = Boolean(selection && brief.trim());
  function submit(event: FormEvent) {
    event.preventDefault();
    if (ready && !request.isPending) request.mutate();
  }
  return <Dialog.Portal>
    <Dialog.Overlay className="conv-dialog-scrim" />
    <Dialog.Content className="conv-dialog">
      <Dialog.Title>Assist with AI</Dialog.Title>
      <Dialog.Description>Ask Codex or Claude to prepare a draft in this worktree for your review.</Dialog.Description>
      {artifact && <div className="break-all">
        <strong>{artifact.name}</strong> · {artifact.version.kind.replaceAll("_", " ")}
        <p>Base revision <code>{artifact.version.revision}</code></p>
      </div>}
      <form className="conv-dialog-form" onSubmit={submit}>
        <ModelPicker label="Authoring model" options={models.options} loading={models.loading} disabled={request.isPending}
          selection={{ mode: "single", selected, onChange: (next) => { setSelected(next); setEffort(""); } }} />
        {model && <EffortSelect capabilities={model} value={effort} onChange={setEffort} label="Authoring effort" />}
        <label>Authoring request<textarea rows={4} value={brief} onChange={(event) => setBrief(event.currentTarget.value)} disabled={request.isPending} /></label>
        {models.errors.map((error, index) => <p key={index} role="alert">{String(error)}</p>)}
        {request.error && <p role="alert">{String(request.error)}</p>}
        <div className="flex justify-end gap-2">
          <Dialog.Close asChild><Button variant="outline" type="button" disabled={request.isPending}>Cancel</Button></Dialog.Close>
          <Button type="submit" disabled={!ready || request.isPending}>
            {request.isPending ? "Starting…" : "Start authoring request"}
          </Button>
        </div>
      </form>
    </Dialog.Content>
  </Dialog.Portal>;
}
