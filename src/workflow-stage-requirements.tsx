import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { Capability } from "@/bindings/Capability";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import type { SessionMode } from "@/bindings/SessionMode";
import { Button } from "@/components/ui/button";
import { ModelPicker } from "@/components/model-picker";
import type { StagePlanConfiguration } from "@/workflow-stage-plan";

type Subject = { kind: "protocol"; capability: Capability } | { kind: "tool"; id: string; version: string; operation: string };
type SourcePath = { kind: "checkout" | "absolute"; path: string };
type Source = { provider: Provider } & (
  { kind: "instruction"; path: SourcePath; revision: string } |
  { kind: "skill"; path: SourcePath; namespace: string | null; revision: string; invoke: boolean } |
  { kind: "plugin"; identity: { id: string; scope?: string; source?: string; marketplace?: string;
    version?: string; sourceLocation?: string; installPath?: string } });
export type StageRequirements = { capabilities?: Subject[]; sources?: Source[] };
type RoleRevision = { templateId: string; revision: string };
export type RequirementConfiguration = { roleTemplate?: RoleRevision; stage?: StageRequirements; plan?: StagePlanConfiguration };
type Decision = { blocker: string | null } & ({kind: "capability"; subject: Subject; status: string | null; evidenceId: string | null} |
  {kind: "source"; source: Source});
type Inventory = {
  provider: Provider; cliVersion: string; choices: { label: string; source: Source }[];
  protocolCapabilities: Capability[]; capabilityPreview: Decision[]; notices: string[];
};
type Assessment = {
  role: string; instanceRevision: string; roleTemplate: RoleRevision | null;
  scope: { provider: Provider; profileId: string; requestedModel: string | null; reportedModel: string | null;
    cliVersion: string; checkout: { path: string } };
  decisions: Decision[];
};
type RoleHead = { templateId: string; version: { revision: string; ordinal: number }; name: string };
type RoleDetails = { revision: { template: { requirements?: StageRequirements } } };
type ProfileMatch = { selection: ModelSelection; attemptId: string | null; checkedAt: string | null; blockers: string[] };
const codexImageGeneration = { kind: "tool", id: "codex-native/imageGeneration", version: "app-server-v2", operation: "generate_image" } as const;

function isCodexImageGeneration(subject: Subject) {
  return subject.kind === "tool" && subject.id === codexImageGeneration.id
    && subject.version === codexImageGeneration.version && subject.operation === codexImageGeneration.operation;
}

function label(value: Subject | Source): string {
  switch (value.kind) {
    case "protocol": return value.capability.replaceAll("_", " ");
    case "tool": return isCodexImageGeneration(value)
      ? "Codex image generation" : `${value.id} · ${value.version} · ${value.operation}`;
    case "plugin": return [value.provider, value.identity.id, value.identity.scope,
      value.identity.marketplace, value.identity.sourceLocation ?? value.identity.installPath, value.identity.version].filter(Boolean).join(" · ");
    case "instruction": return `${value.provider} · ${value.path.path} · ${value.revision}`;
    case "skill": return [value.provider, value.namespace, value.path.path, value.revision].filter(Boolean).join(" · ");
  }
}

function sourceKey(source: Source) {
  return JSON.stringify(source.kind === "skill" ? { ...source, invoke: false } : source);
}

export function WorkflowRequirementsEditor({ projectId, checkoutId, mode, model, options, effort, disabled, configuration, rolePin, onChange }: {
  projectId: string; checkoutId?: string; mode: SessionMode; model: ModelSelection | null; options: readonly ModelOption[];
  effort?: string | null; disabled: boolean; configuration: RequirementConfiguration; rolePin?: boolean;
  onChange: (configuration: RequirementConfiguration) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [previewModel, setPreviewModel] = useState<ModelSelection | null>(null);
  const [sourceIndex, setSourceIndex] = useState("");
  const [protocol, setProtocol] = useState<Capability | "">("");
  const [tool, setTool] = useState({ id: "", version: "", operation: "" });
  const selectedModel = model ?? previewModel;
  const staged = configuration.stage ?? {};
  const sources = staged.sources ?? [];
  const capabilities = staged.capabilities ?? [];
  const pinnedRole = configuration.roleTemplate;
  const roleRequirements = useQuery({
    queryKey: ["conversation-templates", projectId, pinnedRole?.templateId, pinnedRole?.revision],
    queryFn: () => invoke<RoleDetails>("conversation_template_revision", { projectId, ...pinnedRole }),
    enabled: expanded && Boolean(pinnedRole),
    retry: false,
  });
  const inherited = roleRequirements.data?.revision.template.requirements;
  const allSources = [...sources, ...inherited?.sources ?? []];
  const providers = [...new Set(allSources.map((source) => source.provider))];
  const matchingOptions = options.filter((option) => option.selectable &&
    providers.every((provider) => provider === option.selection.provider));
  const candidates = new Map<string, ModelSelection>();
  for (const candidate of [model, previewModel, ...options.filter((option) => option.selectable).map(({ selection }) => selection)]) {
    if (!candidate) continue;
    const key = JSON.stringify([candidate.provider, candidate.profileId]);
    if (!candidates.has(key)) candidates.set(key, candidate);
  }
  const models = [...candidates.values()];
  const roleHeads = useQuery({
    queryKey: ["conversation-templates", projectId],
    queryFn: () => invoke<RoleHead[]>("list_conversation_templates", { projectId }),
    enabled: expanded && Boolean(rolePin) && !disabled,
    retry: false,
  });
  const scopeKey = JSON.stringify([projectId, checkoutId, selectedModel, effort, mode]);
  const configurationKey = JSON.stringify(configuration);
  const preview = useMutation({
    mutationFn: ({ selected }: { selected: ModelSelection; scopeKey: string; configurationKey: string }) => invoke<Inventory>("workflow_stage_inventory", {
      projectId, checkoutId, model: selected, effort: effort ?? null, mode, configuration,
    }),
    onMutate: () => setSourceIndex(""),
  });
  const matchKey = JSON.stringify([scopeKey, configurationKey, models]);
  const profileMatches = useMutation({
    mutationFn: async (_key: string) => {
      const results: ProfileMatch[] = [];
      for (let offset = 0; offset < models.length; offset += 8) {
        results.push(...await invoke<ProfileMatch[]>("workflow_stage_matching_profiles", {
          projectId, checkoutId, models: models.slice(offset, offset + 8), effort: effort ?? null, mode, configuration,
        }));
      }
      return results;
    },
  });
  const matches = profileMatches.variables === matchKey ? profileMatches.data : undefined;
  const inventory = preview.variables?.scopeKey === scopeKey ? preview.data : undefined;
  const capabilityPreview = preview.variables?.configurationKey === configurationKey ? inventory?.capabilityPreview ?? [] : [];
  const missingSources = inventory ? allSources.filter((source) =>
    !inventory.choices.some((choice) => sourceKey(choice.source) === sourceKey(source))) : [];
  const roleChoices = (roleHeads.data ?? []).map((entry) => ({
    key: JSON.stringify({ templateId: entry.templateId, revision: entry.version.revision }),
    label: `${entry.name} · Revision ${entry.version.ordinal}`,
  }));
  const roleKey = configuration.roleTemplate ? JSON.stringify(configuration.roleTemplate) : "";

  function updateStage(stage: StageRequirements) {
    onChange({ ...configuration, stage });
  }

  function addCapability(subject: Subject) {
    if (!capabilities.some((candidate) => JSON.stringify(candidate) === JSON.stringify(subject))) {
      updateStage({ ...staged, capabilities: [...capabilities, subject] });
    }
  }

  function requestPreview() {
    if (selectedModel && checkoutId) preview.mutate({ selected: selectedModel, scopeKey, configurationKey });
  }

  return <details className="pipeline-evidence" onToggle={(event) => setExpanded(event.currentTarget.open)}>
    <summary>{rolePin ? "Role and stage requirements" : "Role requirements"}</summary>
    {rolePin && <label>Pinned role requirements <select disabled={disabled} value={roleKey} onChange={(event) => {
      const { roleTemplate: _previous, ...rest } = configuration;
      const key = event.currentTarget.value;
      onChange(key ? { ...rest, roleTemplate: JSON.parse(key) as RoleRevision } : rest);
    }}>
      <option value="">No pinned role template</option>
      {roleKey && !roleChoices.some(({ key }) => key === roleKey) &&
        <option value={roleKey}>Pinned role revision {configuration.roleTemplate?.revision}</option>}
      {roleChoices.map(({ key, label: name }) => <option key={key} value={key}>{name}</option>)}
    </select></label>}
    {roleHeads.error && <p role="alert">{String(roleHeads.error)}</p>}
    {roleRequirements.error && <p role="alert">{String(roleRequirements.error)}</p>}
    {inherited && <ul aria-label="Pinned role evidence" className="pipeline-stack">
      {[...inherited.capabilities ?? [], ...inherited.sources ?? []].map((entry, index) =>
        <li key={index} className="break-all">{label(entry)}</li>)}
    </ul>}
    {(sources.length > 0 || capabilities.length > 0) && <ul aria-label="Required stage evidence" className="pipeline-stack">
      {sources.map((source, index) => <li key={sourceKey(source)} className="break-all">
        <span>{label(source)}</span>
        {source.kind === "skill" && source.provider === "codex" && <label>
          <input type="checkbox" checked={source.invoke} disabled={disabled || !source.invoke && sources.filter((entry) => entry.kind === "skill" && entry.invoke).length >= 8}
            onChange={(event) => {
              const invokeSkill = event.currentTarget.checked;
              updateStage({ ...staged, sources: sources.map((entry, selected) => selected === index ? { ...source, invoke: invokeSkill } : entry) });
            }} /> Invoke this skill
        </label>}
        {!disabled && <Button type="button" variant="outline" onClick={() =>
          updateStage({ ...staged, sources: sources.filter((_, selected) => selected !== index) })}>Remove source</Button>}
      </li>)}
      {capabilities.map((subject, index) => <li key={label(subject)}>
        <span>{label(subject)}</span>
        {!disabled && <Button type="button" variant="outline" onClick={() =>
          updateStage({ ...staged, capabilities: capabilities.filter((_, selected) => selected !== index) })}>Remove capability</Button>}
      </li>)}
    </ul>}
    {!disabled && <div className="pipeline-stack">
      {expanded && !model && <ModelPicker label="Preview profile and model" options={matchingOptions}
        selection={{ mode: "single", selected: previewModel, onChange: setPreviewModel }} />}
      {providers.length > 0 && !matchingOptions.length && <p>No available model uses every selected source provider.</p>}
      {selectedModel && providers.some((provider) => provider !== selectedModel.provider) &&
        <p role="alert">The selected model uses a different provider from a required source.</p>}
      <Button variant="outline" type="button" disabled={!checkoutId || !selectedModel || preview.isPending} onClick={requestPreview}>
        {preview.isPending ? "Checking worktree inventory…" : "Preview worktree inventory"}
      </Button>
      {preview.error && preview.variables?.scopeKey === scopeKey && <p role="alert">{String(preview.error)}</p>}
      <Button variant="outline" type="button" disabled={!checkoutId || !models.length || profileMatches.isPending ||
        !configuration.roleTemplate && !sources.length && !capabilities.length && configuration.plan?.questionRoute !== "native"}
        onClick={() => profileMatches.mutate(matchKey)}>
        {profileMatches.isPending ? "Checking saved profile assessments…" : "Find matching profiles"}
      </Button>
      {profileMatches.error && profileMatches.variables === matchKey && <p role="alert">{String(profileMatches.error)}</p>}
      {matches && <div className="pipeline-stack">
        <p>Saved assessments for the current checkout. Dispatch checks the live session again.</p>
        <ul aria-label="Evaluated CLI profiles">{matches.map((entry) => <li key={JSON.stringify(entry.selection)}>
          <strong>{entry.selection.provider} · {entry.selection.profileId} · {entry.selection.modelId}</strong>
          {entry.attemptId && <p>Attempt <code>{entry.attemptId}</code> · <time dateTime={entry.checkedAt ?? undefined}>{entry.checkedAt}</time></p>}
          {entry.blockers.length === 0 ? <p>Verified match</p> : <ul>{entry.blockers.map((reason, index) => <li key={index}>{reason}</li>)}</ul>}
        </li>)}</ul>
      </div>}
      {inventory && <>
        <p>{inventory.provider} · CLI {inventory.cliVersion}</p>
        {inventory.notices.map((notice) => <p key={notice}>{notice}</p>)}
        {missingSources.length > 0 && <ul aria-label="Sources missing from this preview">
          {missingSources.map((source, index) => <li key={index} className="break-all">{label(source)} · Exact source absent from worktree preview</li>)}
        </ul>}
        {capabilityPreview.length > 0 && <ul aria-label="Selected profile capability preview">
          {capabilityPreview.map((decision) => decision.kind === "capability" &&
            <li key={label(decision.subject)}>{label(decision.subject)} · {decision.status ?? "Unconfirmed in preview"}
              {decision.blocker && ` · ${decision.blocker}`}</li>)}
        </ul>}
        <label>Instruction, skill, or plugin source <select value={sourceIndex} onChange={(event) => setSourceIndex(event.currentTarget.value)}>
          <option value="">Choose an exact source</option>
          {inventory.choices.map(({ label: name, source }, index) =>
            <option key={index} value={index} disabled={sources.some((entry) => sourceKey(entry) === sourceKey(source))}>{name}</option>)}
        </select></label>
        <Button variant="outline" type="button" disabled={!sourceIndex || sources.length >= 32}
          onClick={() => {
            const choice = inventory.choices[Number(sourceIndex)];
            if (!choice) return;
            updateStage({ ...staged, sources: [...sources, choice.source] });
            setSourceIndex("");
          }}>Require source</Button>
        <label>Protocol capability <select value={protocol} onChange={(event) => setProtocol(event.currentTarget.value as Capability | "")}>
          <option value="">Choose a capability</option>
          {inventory.protocolCapabilities.map((capability) => <option key={capability} value={capability}>{capability.replaceAll("_", " ")}</option>)}
        </select></label>
        <Button variant="outline" type="button" disabled={!protocol || capabilities.length >= 32}
          onClick={() => { if (protocol) addCapability({ kind: "protocol", capability: protocol }); }}>Require capability</Button>
      </>}
      <Button variant="outline" type="button" disabled={capabilities.length >= 32 || capabilities.some(isCodexImageGeneration)}
        onClick={() => addCapability(codexImageGeneration)}>Require Codex image generation</Button>
      <details><summary>Require an exact tool operation</summary>
        <label>Tool ID <input value={tool.id} maxLength={1024} onChange={(event) => setTool({ ...tool, id: event.currentTarget.value })} /></label>
        <label>Tool version <input value={tool.version} maxLength={1024} onChange={(event) => setTool({ ...tool, version: event.currentTarget.value })} /></label>
        <label>Tool operation <input value={tool.operation} maxLength={1024} onChange={(event) => setTool({ ...tool, operation: event.currentTarget.value })} /></label>
        <Button variant="outline" type="button" disabled={!tool.id.trim() || !tool.version.trim() || !tool.operation.trim() || capabilities.length >= 32}
          onClick={() => addCapability({ kind: "tool", id: tool.id.trim(), version: tool.version.trim(), operation: tool.operation.trim() })}>Require tool operation</Button>
      </details>
    </div>}
  </details>;
}

export function AttemptStageRequirements({ projectId, runId }: { projectId: string; runId: string }) {
  const [expanded, setExpanded] = useState(false);
  const assessment = useQuery({
    queryKey: ["workflow-stage-assessment", projectId, runId],
    queryFn: () => invoke<Assessment | null>("workflow_stage_assessment", { projectId, runId }),
    enabled: expanded,
    retry: false,
  });
  const data = assessment.data;
  return <details onToggle={(event) => setExpanded(event.currentTarget.open)}>
    <summary>Stage requirement assessment</summary>
    {assessment.isFetching && <p role="status">Loading saved assessment…</p>}
    {assessment.error && <p role="alert">{String(assessment.error)}</p>}
    {data === null && <p>This attempt has no saved stage requirement assessment.</p>}
    {data && <>
      <p>{data.role} · <code>{data.instanceRevision}</code></p>
      {data.roleTemplate && <p>Pinned role revision <code>{data.roleTemplate.revision}</code></p>}
      <p>{data.scope.provider} · {data.scope.profileId} · {data.scope.reportedModel ?? data.scope.requestedModel} · CLI {data.scope.cliVersion}</p>
      <p>{data.scope.checkout.path}</p>
      <ul className="pipeline-stack">{data.decisions.map((decision, index) => <li key={index} className="break-all">
        {label(decision.kind === "capability" ? decision.subject : decision.source)} · {decision.blocker ?? "Verified for dispatch"}
        {decision.kind === "capability" && decision.evidenceId && <span> · Evidence <code>{decision.evidenceId}</code></span>}
      </li>)}</ul>
    </>}
  </details>;
}
