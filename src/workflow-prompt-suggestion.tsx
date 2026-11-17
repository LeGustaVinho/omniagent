import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Sparkles } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ModelPicker } from "@/components/model-picker";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { availableModelSelection, useLiveModelOptions } from "@/live-models";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";
import { portType } from "@/workflow-graph-model";
import type { PromptContract, PromptProposal } from "@/workflow-prompt-model";

const providers = ["codex", "claude"] as const;
type Suggestion = { state: "pending" | "ready" | "failed"; proposal?: PromptProposal; diagnostic?: string };

export function PromptContractSuggestion({ projectId, checkoutId, prompt, cardFingerprint, disabled, onApply }: {
  projectId: string;
  checkoutId: string;
  prompt: PromptContract;
  cardFingerprint: string;
  disabled: boolean;
  onApply: (proposal: PromptProposal) => void;
}) {
  const [open, show] = useState(false);
  const [selection, select] = useState<ModelSelection | null>(null);
  const [request, setRequest] = useState<{ jobId: string; snapshot: PromptContract; fingerprint: string }>();
  const models = useLiveModelOptions(providers, open);
  const model = availableModelSelection(models.options, selection);
  const start = useMutation({
    mutationFn: ({ snapshot }: { snapshot: PromptContract; fingerprint: string }) => invoke<string>("request_prompt_contract_suggestion", {
      projectId, checkoutId, selection: model, effort: null, prompt: snapshot,
    }),
    onSuccess: (jobId, pending) => setRequest({ jobId, ...pending }),
  });
  const result = useQuery({
    queryKey: ["workflow-prompt-contract-suggestion", projectId, request?.jobId],
    queryFn: () => invoke<Suggestion>("prompt_contract_suggestion", { projectId, jobId: request?.jobId }),
    enabled: !!request && open,
    refetchInterval: (query) => (!query.state.data || query.state.data.state === "pending" ? 1000 : false),
    retry: false,
  });
  const stale = request && request.fingerprint !== cardFingerprint;
  const waiting = start.isPending || (!!request && !result.error && (!result.data || result.data.state === "pending"));
  const proposal = result.data?.state === "ready" ? result.data.proposal : undefined;
  const error = start.error ?? result.error;
  return <WorkflowBuilderDialog title="Suggest contract with AI"
    description="Review proposed ports and routes before applying them to this card."
    open={open} onOpenChange={show}
    trigger={<Button type="button" variant="outline" className="w-fit" disabled={disabled || !prompt.text.trim()}>
      <Sparkles size={15} aria-hidden /> Suggest with AI
    </Button>}>
    <div className="pipeline-form pipeline-stack">
      <ModelPicker label="Suggestion model" options={models.options} loading={models.loading} disabled={waiting || disabled}
        selection={{ mode: "single", selected: selection, onChange: select }} />
      {models.errors[0] && <p role="alert">{String(models.errors[0])}</p>}
      <Button type="button" variant="outline" className="w-fit" disabled={disabled || !prompt.text.trim() || !model || waiting}
        onClick={() => { setRequest(undefined); start.mutate({ snapshot: prompt, fingerprint: cardFingerprint }); }}>
        <Sparkles size={15} aria-hidden /> Request suggestions
      </Button>
      {waiting && <p role="status">Preparing a contract proposal…</p>}
      {error && <p role="alert">{String(error)}</p>}
      {result.data?.state === "failed" && <p role="alert">{result.data.diagnostic || "The suggestion job could not produce a contract."}</p>}
      {stale && <p role="status">This card changed after the request. Request a new proposal to apply it.</p>}
      {proposal && request && <>
        <ContractProposal previous={request.snapshot} proposal={proposal} />
        <Button type="button" disabled={disabled || stale} onClick={() => {
          if (stale) return;
          onApply(proposal);
          setRequest(undefined);
          show(false);
        }}>Apply proposed contract</Button>
      </>}
    </div>
  </WorkflowBuilderDialog>;
}

function ContractProposal({ previous, proposal }: { previous: PromptContract; proposal: PromptProposal }) {
  type Port = PromptContract["inputs"][string] & PromptContract["outputs"][string];
  const description = ({ kind, target, source }: Port) => {
    const type = portType({ kind: "data", artifact: kind });
    return [type, target && "to " + target, source && "from " + source].filter(Boolean).join(" · ");
  };
  function changes(before: Record<string, Port>, next: Record<string, Port>) {
    return [...new Set([...Object.keys(before), ...Object.keys(next)])].flatMap((name) => {
      const old = Object.hasOwn(before, name) ? before[name] : undefined;
      const proposed = Object.hasOwn(next, name) ? next[name] : undefined;
      if (JSON.stringify(old) === JSON.stringify(proposed)) return [];
      return [{ name, description: proposed ? description(proposed) : "Removed", previous: old && proposed ? description(old) : undefined }];
    });
  }
  const routesChanged = JSON.stringify(previous.routes) !== JSON.stringify(proposal.routes);
  const schemaChanged = proposal.outputSchema !== undefined && JSON.stringify(proposal.outputSchema) !== JSON.stringify(previous.outputSchema);
  return <div className="workflow-prompt-proposal" aria-label="Proposed Prompt contract">
    <h3>Proposed changes</h3>
    {([["Data inputs", changes(previous.inputs, proposal.inputs)], ["Data outputs", changes(previous.outputs, proposal.outputs)]] as const)
      .map(([name, entries]) => entries.length > 0 && <section key={name}>
        <h4>{name}</h4>
        <ul>{entries.map((entry) => <li key={entry.name}><strong>{entry.name}</strong> · {entry.description}
          {entry.previous && <small className="block">Before · {entry.previous}</small>}</li>)}</ul>
      </section>)}
    {routesChanged && <p><strong>Routes</strong> · {proposal.routes.join(", ")}</p>}
    {schemaChanged && <details>
      <summary>Structured response schema</summary>
      <pre className="max-h-48 overflow-auto whitespace-pre-wrap text-xs">{JSON.stringify(proposal.outputSchema, null, 2)}</pre>
    </details>}
    {!routesChanged && !schemaChanged && [changes(previous.inputs, proposal.inputs), changes(previous.outputs, proposal.outputs)].every((item) => !item.length)
      && <p>The proposal matches the current contract.</p>}
  </div>;
}
