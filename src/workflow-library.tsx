import { useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Braces, Layers3, Plus, Search, UserRound, Waypoints, Workflow } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ConversationTemplateEditor, emptyRoleDraft } from "@/conversation-template-editor";
import { LegacyPipelineDrafts, PipelineAuthoringEditor } from "@/workflow-authoring";
import { PipelineSectionHeading } from "@/pipeline-layout";
import { newPipelineMember, schemaMember, WorkflowPackageEditor, WorkflowTemplateChooser,
  type PackageDraft, type PackageMemberSummary, type PackageSummary, type WorkflowMember } from "@/workflow-authoring-package";
import { WorkflowArtifactViewer } from "@/workflow-package-review";
import { WorkflowDraftRow } from "@/workflow-draft-row";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";
import type { WorkflowCatalogItem } from "@/workflow-node-picker";
import { defaultTypedPrompt } from "@/workflow-prompt-model";
import { publishedWorkflowArtifactQuery } from "@/workflow-reusable";
import { workflowTemplates } from "@/workflow-templates";
import type { WorkspaceProps } from "@/workbench-contract";

export const libraryCategories = [
  { id: "pipeline", label: "Pipeline", title: "Pipeline", icon: Workflow, kind: "pipeline" },
  { id: "functions", label: "Functions", title: "Function", icon: Waypoints, kind: "subflow" },
  { id: "schemas", label: "Schemas", title: "Schema", icon: Braces, kind: "schema" },
  { id: "prefabs", label: "Node Prefabs", title: "Node Prefab", icon: Layers3, kind: "prefab" },
  { id: "roles", label: "Roles", title: "Role", icon: UserRound, kind: "role" },
] as const;

export type LibraryCategory = typeof libraryCategories[number]["id"];
type DraftKind = "pipeline" | "schema" | "prefab" | "function";
type DraftMember = WorkflowMember;
type NativeTemplate = typeof workflowTemplates[number];
type Screen =
  | { type: "draft"; id: string; memberKey?: string }
  | { type: "legacy"; id: string }
  | { type: "published"; item: WorkflowCatalogItem }
  | { type: "native"; template: NativeTemplate; memberKey: string };

function memberKind({ version, nodePrefab, prompt, typedPrompt }: WorkflowCatalogItem) {
  return nodePrefab ? "prefab" : prompt || typedPrompt ? "prompt" : version.kind;
}

function draftKind({ kind, nodePrefab, prompt }: PackageMemberSummary) {
  return nodePrefab ? "prefab" : prompt ? "prompt" : kind;
}

function nativeMembers(template: NativeTemplate): WorkflowMember[] {
  return template.members.map(({ key, name, body, ...member }) => {
    const { template: role, ...content } = body;
    return { key, name, body: { ...content, ...(role && { template: { ...emptyRoleDraft(), ...role } }) },
      references: "references" in member ? member.references : undefined };
  });
}

function newMember(kind: DraftKind, name: string): DraftMember {
  switch (kind) {
    case "pipeline": return newPipelineMember(name);
    case "schema": return schemaMember(kind, name);
    case "prefab": return { key: kind, name, body: { kind: "prompt", prompt: { ...defaultTypedPrompt(), nodePrefab: true } } };
    case "function": return { key: kind, name, body: {
      kind: "subflow", graph: { name, nodes: [
        { id: "start", kind: "start", configuration: { kind: "subflow_entry", outputs: [
          { name: "ready", value: { kind: "control" } },
          { name: "inputs", value: { kind: "data", artifact: "json_object" } },
        ] } },
        { id: "return", kind: "end", configuration: { kind: "end" } },
      ], wires: [
        { from: { node: "start", port: "ready" }, to: { node: "return", port: "ready" } },
        { from: { node: "start", port: "inputs" }, to: { node: "return", port: "result" } },
      ] },
      inputs: { ready: { node: "start", port: "ready" }, input: { node: "start", port: "inputs" } },
      outputs: { completed: { node: "return", port: "completed" }, result: { node: "return", port: "result" } },
    } };
  }
}

export function WorkflowLibrary({ category, ...props }: WorkspaceProps & { category: LibraryCategory }) {
  const { checkout, openWorkspace } = props;
  const { projectId, checkoutId } = checkout;
  const client = useQueryClient();
  const section = libraryCategories.find(({ id }) => id === category)!;
  const [screen, show] = useState<Screen>();
  const [filter, setFilter] = useState("");
  const [create, setCreate] = useState(false);
  const [name, setName] = useState("");
  const list = useRef<HTMLDivElement>(null);
  const scroll = useRef(0);
  const origin = useRef<HTMLElement | null>(null);
  const catalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId],
    queryFn: () => invoke<WorkflowCatalogItem[]>("workflow_package_catalog", { projectId }),
  });
  const drafts = useQuery({
    queryKey: ["workflow-packages", projectId],
    queryFn: () => invoke<PackageSummary[]>("list_workflow_packages", { projectId }),
    select: (items) => items.filter(({ published }) => !published),
  });

  function enter(next: Screen) {
    if (!screen) {
      scroll.current = list.current?.scrollTop ?? 0;
      origin.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    }
    show(next);
  }

  function back() {
    show(undefined);
    edit.reset();
    requestAnimationFrame(() => {
      if (list.current) list.current.scrollTop = scroll.current;
      const target = origin.current?.isConnected && !origin.current.closest("[data-state='closed']")
        ? origin.current : list.current?.querySelector<HTMLInputElement>("input[type='search']");
      target?.focus({ preventScroll: true });
    });
  }

  function openDraft(result: PackageDraft, memberKey?: string) {
    client.setQueryData(["workflow-packages", projectId, result.id], result);
    void client.invalidateQueries({ queryKey: ["workflow-packages", projectId], exact: true });
    enter({ type: "draft", id: result.id, memberKey });
  }

  const edit = useMutation({
    mutationFn: async (request: { item: WorkflowCatalogItem } | { kind: DraftKind; name: string }) => {
      let member: DraftMember;
      if ("item" in request) {
        const { version } = request.item;
        const artifact = await client.fetchQuery(publishedWorkflowArtifactQuery(projectId, version));
        if (!artifact) throw new Error("The selected revision is unavailable.");
        member = { key: "artifact", name: artifact.name, body: artifact.body, references: artifact.references,
          base: version, destination: version.destination };
      } else {
        member = newMember(request.kind, request.name);
      }
      const draft = await invoke<PackageDraft>("create_workflow_package", {
        projectId, conversationId: null, package: { name: member.name, members: [member] },
      });
      return { draft, memberKey: member.key };
    },
    onSuccess: ({ draft, memberKey }) => { setCreate(false); openDraft(draft, memberKey); },
  });

  const query = filter.trim().toLowerCase();
  const matches = (name: string) => name.toLowerCase().includes(query);
  const published = (catalog.data ?? []).filter((item) => memberKind(item) === section.kind && matches(item.name)
    && (category !== "roles" || item.version.destination === "library"));
  const categorizedDrafts = (drafts.data ?? []).flatMap<{ draft: PackageSummary; member?: PackageMemberSummary; label: string }>((draft) => {
    const members = draft.members ?? [];
    const entries = members.filter((member) => draftKind(member) === section.kind && (matches(member.name) || matches(draft.name)));
    if (entries.length) return entries.map((member) => ({ draft, member, label: member.name }));
    if (category === "pipeline" && !members.some((member) => libraryCategories.some(({ kind }) => kind === draftKind(member))) && matches(draft.name)) {
      return [{ draft, member: undefined, label: draft.name }];
    }
    return [];
  });
  const creation = section.kind === "subflow" ? "function" : section.kind;
  const intro = category === "roles" ? "Shared Roles" : section.label;
  const heading = <PipelineSectionHeading title={intro} icon={<section.icon size={20} aria-hidden />} />;
  const error = catalog.error ?? drafts.error ?? (!create && !screen ? edit.error : null);
  return <>
    <div ref={list} hidden={!!screen} className="workflow-library-list">
      {category === "roles" && <ConversationTemplateEditor checkout={checkout} openWorkspace={openWorkspace} />}
      <section className="pipeline-view-content pipeline-stack" aria-label="Workflow library">
        <header className="pipeline-actions justify-between">
          {heading}
          {creation !== "role" && <WorkflowBuilderDialog title={`New ${section.title}`} description={`Name your ${section.title.toLowerCase()} to open its editor.`}
            open={create} onOpenChange={(next) => {
              if (edit.isPending) return;
              if (next) { edit.reset(); setName(""); }
              setCreate(next);
            }} trigger={<Button type="button"><Plus size={15} aria-hidden /> New {section.title}</Button>}>
            <form className="pipeline-stack pipeline-form" onSubmit={(event) => {
              event.preventDefault();
              if (name.trim() && !edit.isPending) edit.mutate({ kind: creation, name: name.trim() });
            }}>
              <label>Name<input value={name} disabled={edit.isPending} onChange={(event) => setName(event.currentTarget.value)}
                required maxLength={120} autoComplete="off" /></label>
              <Button type="submit" className="w-fit" disabled={!name.trim() || edit.isPending}>Create {section.title}</Button>
              {edit.error && <p role="alert">{String(edit.error)}</p>}
            </form>
          </WorkflowBuilderDialog>}
        </header>
        <label className="work-search workflow-library-search"><Search size={15} aria-hidden />
          <input type="search" aria-label={`Search ${intro}`} placeholder={`Search ${intro.toLowerCase()}`} value={filter}
            onChange={(event) => setFilter(event.currentTarget.value)} /></label>
        {error && <p role="alert">{String(error)}</p>}
        <section className="workflow-library-section" aria-label="Published revisions">
          <h3>Published</h3>
          {catalog.isPending ? <p role="status">Loading published revisions…</p> : published.length ? <ul>{published.map((item) =>
            <li key={JSON.stringify(item.version)}>
              <div><strong>{item.name}</strong><code>{item.version.revision.slice(0, 10)}</code></div>
              <div className="workflow-library-actions">
                <Button type="button" variant="outline" aria-label={`View ${item.name}`} onClick={() => enter({ type: "published", item })}>View</Button>
                <Button type="button" variant="outline" aria-label={`Edit ${item.name}`} disabled={edit.isPending}
                  onClick={() => edit.mutate({ item })}>Edit</Button>
              </div>
              {!!item.blockers.length && <small>{item.blockers.join(" ")}</small>}
            </li>)}</ul> : <p className="pipeline-meta">No published {intro.toLowerCase()}.</p>}
        </section>
        <section className="workflow-library-section" aria-label="Draft revisions">
          <h3>Drafts</h3>
          {drafts.isPending ? <p role="status">Loading drafts…</p> : categorizedDrafts.length ? <ul>{categorizedDrafts.map(({ draft, member, label }) => {
            const open = () => enter({ type: "draft", id: draft.id, memberKey: member?.key });
            return category === "pipeline" && member?.kind === "pipeline" ?
              <WorkflowDraftRow key={`${draft.id}:${member.key}`} projectId={projectId} draftId={draft.id} kind="package"
                name={label} revision={draft.revision} memberCount={draft.memberCount} open={open} openLabel="Open draft"
                openAriaLabel={`Open draft of ${label}`} /> :
              <li key={`${draft.id}:${member?.key ?? ""}`}><strong>{label}</strong>
                <Button type="button" variant="outline" aria-label={`Open draft of ${label}`} onClick={open}>Open draft</Button></li>;
          })}</ul> : <p className="pipeline-meta">No drafts.</p>}
        </section>
        {category === "pipeline" && <LegacyPipelineDrafts checkout={checkout} openWorkspace={openWorkspace} onlyUnpublished
          onOpen={(id) => enter({ type: "legacy", id })} />}
        <section className="workflow-library-section" aria-label="Native templates">
          <h3>Native templates</h3>
          {workflowTemplates.map((template) => {
            const entries = template.members.filter(({ kind, name }) => kind === section.kind && matches(name));
            const prompts = category === "pipeline" ? template.members.filter(({ kind }) => kind === "prompt") : [];
            return entries.length > 0 && <article key={template.key} className="workflow-native-template" aria-label={template.name}>
              <header className="workflow-native-template-heading"><Workflow size={20} aria-hidden /><h4>{template.name}</h4>
                {category === "pipeline" && <WorkflowTemplateChooser projectId={projectId} initialTemplateKey={template.key} onCreated={openDraft} />}
              </header>
              <ul>{entries.map(({ key, name }) => <li key={key}><strong>{name}</strong>
                <Button type="button" variant="outline" aria-label={`View ${name}`}
                  onClick={() => enter({ type: "native", template, memberKey: key })}>View</Button></li>)}</ul>
              {prompts.length > 0 && <details><summary>Prompts</summary><ul>{prompts.map(({ key, name }) =>
                <li key={key}><strong>{name}</strong><Button type="button" variant="outline" aria-label={`View ${name}`}
                  onClick={() => enter({ type: "native", template, memberKey: key })}>View</Button></li>)}</ul></details>}
            </article>;
          })}
        </section>
      </section>
    </div>
    {screen?.type === "draft" && <WorkflowPackageEditor key={screen.id} {...props} draftId={screen.id} memberKey={screen.memberKey} onBack={back} />}
    {screen?.type === "legacy" && <PipelineAuthoringEditor key={screen.id} {...props} draftId={screen.id} onBack={back} />}
    {screen?.type === "published" && <WorkflowArtifactViewer projectId={projectId} checkoutId={checkoutId}
      source={{ version: screen.item.version }} onBack={back} actions={<>
        <Button type="button" disabled={edit.isPending} onClick={() => edit.mutate({ item: screen.item })}>Edit</Button>
        {edit.error && <p role="alert">{String(edit.error)}</p>}
      </>} />}
    {screen?.type === "native" && <WorkflowArtifactViewer projectId={projectId} checkoutId={checkoutId}
      source={{ key: screen.memberKey }} members={nativeMembers(screen.template)} onBack={back}
      actions={<WorkflowTemplateChooser projectId={projectId} initialTemplateKey={screen.template.key}
        onCreated={(draft) => openDraft(draft, screen.memberKey)} />} />}
  </>;
}
