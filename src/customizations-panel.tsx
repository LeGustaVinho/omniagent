import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useContext, useState } from "react";
import type { CustomizationEntry } from "@/bindings/CustomizationEntry";
import type { CustomizationInventory } from "@/bindings/CustomizationInventory";
import type { CustomizationKind } from "@/bindings/CustomizationKind";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { Provider } from "@/bindings/Provider";
import type { ReviewState } from "@/bindings/ReviewState";
import { activeCustomizationDraft, useActiveCustomizationDraft } from "@/active-customization-draft";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback, type MenuAction } from "@/components/ui/context-actions";
import { composerCustomizationsQuery, type ComposerCustomization } from "@/conversation-customization-picker";
import { queueConversationDraftWrite } from "@/conversation-draft-writes";
import { markdownExcerpt } from "@/editor-context";
import { openWorkspaceFile } from "@/file-context-actions";
import { RunInspectorNavigation } from "@/run-inspector-navigation";
import { useWorkspace } from "@/workspace-state";

const PROVIDER: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Antigravity" };
const KIND: Record<CustomizationKind, string> = {
  hook: "Hook", mcp_server: "MCP server", plugin: "Plugin", skill_command: "Skill command", helper: "Helper",
};
const REVIEW: Record<ReviewState, string> = {
  pending: "Pending review", changed: "Changed since approval", approved: "Revision approved",
  disabled: "Disabled in declaration", unresolved: "Needs resolution",
};

function inventoryKey(checkoutId: string | undefined) {
  return ["checkout-customizations", checkoutId];
}

function localPath(source: string) {
  const normalized = source.replaceAll("\\", "/");
  if (normalized.includes(":") || normalized.split("/").some((part) => !part || part === "." || part === "..")) return;
  return normalized;
}

function skillCheckoutPath(root: string, source: string) {
  const normalize = (path: string) => path.replaceAll("\\", "/").replace(/^\/\/\?\/UNC\//i, "//").replace(/^\/\/\?\//, "");
  const normalized = normalize(source);
  const prefix = normalize(root).replace(/\/+$/, "") + "/";
  if (normalized.toLowerCase().startsWith(prefix.toLowerCase())) return localPath(normalized.slice(prefix.length));
}

export function useCustomizationInventory(checkoutId: string | undefined) {
  return useQuery({
    queryKey: inventoryKey(checkoutId),
    queryFn: () => invoke<CustomizationInventory>("review_checkout_customizations", { checkoutId }),
    enabled: Boolean(checkoutId),
    staleTime: 15_000,
  });
}

export function CustomizationsPanel() {
  const checkout = useWorkspace((scope) => scope.checkout);
  const openWorkspace = useContext(RunInspectorNavigation)?.openWorkspace;
  return <CustomizationsSettings checkout={checkout}
    onOpenSource={openWorkspace && ((path) => openWorkspaceFile(openWorkspace, path, undefined, { repeat: true }))} />;
}

export function CustomizationsSettings({ checkout, embedded = false, onOpenSource }: {
  checkout: ProjectCheckout | null;
  embedded?: boolean;
  onOpenSource?: (path: string) => void;
}) {
  const checkoutId = checkout?.checkoutId;
  const client = useQueryClient();
  const inventory = useCustomizationInventory(checkoutId);
  const action = useContextActionFeedback();
  const draft = useActiveCustomizationDraft(checkoutId);
  const [catalogRequest, setCatalogRequest] = useState<string>();
  const catalog = composerCustomizationsQuery(draft?.conversationId ?? "", draft?.selection ?? null, draft?.effort ?? null, draft?.mode ?? "read_only");
  const catalogScope = JSON.stringify(catalog.queryKey);
  const canBrowseSkills = draft?.selection?.provider === "codex";
  const showCatalog = canBrowseSkills && catalogRequest === catalogScope;
  const skills = useQuery({ ...catalog, enabled: showCatalog });
  const approve = useMutation({
    mutationFn: ({ id, fingerprint }: CustomizationEntry) =>
      invoke<CustomizationInventory>("approve_checkout_customization", { checkoutId, entryId: id, fingerprint }),
    onSuccess: (result) => client.setQueryData(inventoryKey(checkoutId), result),
    onError: () => { void inventory.refetch(); },
  });
  const data = inventory.data;

  function sourceActions(path: string | undefined, name: string): MenuAction[] {
    if (!checkoutId || !path) return [];
    return [
      ...(onOpenSource ? [{ label: `Open ${name} in workbench`, run: () => onOpenSource(path) }] : []),
      { label: `Show ${name} in File Explorer`, run: () => void action.run(() => invoke("open_checkout_item_in_shell", {
        checkoutId, relativePath: path, action: "reveal",
      })) },
    ];
  }

  function appendToDraft(context: string, skill?: ComposerCustomization) {
    if (!draft || !checkoutId) return;
    const { conversationId, title } = draft;
    void action.run(async () => {
      function verifyDestination() {
        if (activeCustomizationDraft(checkoutId) !== draft) throw new Error("The active conversation or its draft changed. Open the action again.");
      }
      await queueConversationDraftWrite(conversationId, async () => {
        verifyDestination();
        if (skill) {
          const current = await invoke<string>("conversation_draft", { conversationId });
          verifyDestination();
          if (current.split(/\s+/).some((token) => token.startsWith(`@skill:${skill.skill.id}:`))) {
            throw new Error("This skill is already in the active conversation draft.");
          }
        }
        await invoke("append_checkout_context_to_conversation", { checkoutId, conversationId, context });
      });
      await client.invalidateQueries({ queryKey: ["conversation", conversationId, "draft"] });
    }, `Added to the draft for ${title}.`);
  }

  return (
    <section className={embedded ? undefined : "h-full overflow-auto p-6"} data-setting="customization-inventory">
      <div className="mb-4 flex max-w-3xl items-center justify-between gap-3">
        <h1 className="text-xl font-semibold tracking-tight">Repository customizations</h1>
        <Button variant="outline" disabled={!checkoutId || inventory.isFetching || approve.isPending}
          onClick={() => { approve.reset(); void inventory.refetch(); }}>Refresh</Button>
      </div>
      {!checkoutId && <p className="text-sm text-muted-foreground">Open a checkout to inspect its customizations.</p>}
      {checkoutId && inventory.isPending && <p role="status" className="text-sm text-muted-foreground">Reading repository files…</p>}
      {inventory.isError && <p role="alert" className="text-sm text-red-300">{String(inventory.error)}</p>}
      {approve.isError && <p role="alert" className="mb-3 text-sm text-red-300">{String(approve.error)}</p>}
      {action.feedback && <p role={action.feedback.failed ? "alert" : "status"}
        className={action.feedback.failed ? "mb-3 text-sm text-red-300" : "mb-3 text-sm text-muted-foreground"}>{action.feedback.text}</p>}
      {data && (
        <div className="max-w-3xl space-y-6">
          <p className="break-all text-xs text-muted-foreground">{data.workingDirectory}</p>
          {data.unresolvedSources.length > 0 && (
            <details className="rounded-lg border border-border p-3 text-sm">
              <summary className="cursor-pointer font-medium">Sources that could not be inspected ({data.unresolvedSources.length})</summary>
              <ul className="mt-2 space-y-1 font-mono text-xs text-muted-foreground">
                {data.unresolvedSources.map((source) => <li key={source}>
                  <ContextActions label={source} groups={[[{ label: "Copy unresolved path", run: () => action.copy(source) }]]}>
                    <span className="break-all">{source}</span>
                  </ContextActions>
                </li>)}
              </ul>
            </details>
          )}
          <section aria-label="Executable entries">
            <h2 className="mb-2 text-sm font-semibold">Executable entries</h2>
            {data.entries.length === 0 && <p className="text-sm text-muted-foreground">No executable declarations found.</p>}
            <div className="space-y-2">
              {data.entries.map((entry) => (
                <ContextActions key={entry.id} label={`${entry.label} · ${entry.source}`} className="rounded-lg border border-border bg-card p-3 text-sm" groups={[
                  sourceActions(localPath(entry.source), "declaration"),
                  [{ label: "Copy declaration path", run: () => action.copy(entry.source) },
                    { label: "Copy source revision", run: () => action.copy(entry.sourceRevision) },
                    { label: "Copy fingerprint", run: () => action.copy(entry.fingerprint) },
                    { label: "Copy declaration metadata", run: () => action.copy(JSON.stringify(entry, null, 2)) }],
                  entry.localFiles.map((file) => ({ label: file.path, items: [
                    ...sourceActions(localPath(file.path), "local file"),
                    { label: "Copy local file path", run: () => action.copy(file.path) },
                    { label: "Copy local file revision", run: () => action.copy(file.revision) },
                  ] })),
                ]}>
                  <details className="min-w-0 grow">
                    <summary className="cursor-pointer">
                      <span className="font-medium">{entry.label}</span>
                      <span className="ml-2 text-xs text-muted-foreground">{PROVIDER[entry.provider]} · {KIND[entry.kind]} · {REVIEW[entry.review]}</span>
                    </summary>
                    <dl className="mt-3 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-xs">
                      <dt className="text-muted-foreground">Source</dt>
                      <dd className="break-all font-mono" title={entry.sourceRevision}>{entry.source} · {entry.sourceRevision.slice(0, 12)}</dd>
                      <dt className="text-muted-foreground">Target</dt><dd className="break-all font-mono">{entry.target ?? "Unresolved"}</dd>
                      <dt className="text-muted-foreground">Requested scope</dt><dd className="break-words">{entry.requestedScope}</dd>
                      {entry.arguments.length > 0 && <>
                        <dt className="text-muted-foreground">Arguments</dt>
                        <dd className="break-words font-mono">{entry.arguments.map((argument) => JSON.stringify(argument)).join(" ")}</dd>
                      </>}
                      {entry.localFiles.length > 0 && <>
                        <dt className="text-muted-foreground">Local files</dt>
                        <dd className="space-y-1 break-all font-mono">{entry.localFiles.map((file) => <div key={file.path}>{file.path} · {file.revision.slice(0, 12)}</div>)}</dd>
                      </>}
                      <dt className="text-muted-foreground">Revision</dt><dd className="break-all font-mono">{entry.fingerprint}</dd>
                    </dl>
                    {entry.issue && <p className="mt-2 text-xs text-muted-foreground">{entry.issue}</p>}
                    {(entry.review === "pending" || entry.review === "changed") && (
                      <Button className="mt-3 px-3 text-xs" disabled={approve.isPending}
                        onClick={() => approve.mutate(entry)}>Approve revision</Button>
                    )}
                  </details>
                </ContextActions>
              ))}
            </div>
          </section>
          <section aria-label="Markdown instructions">
            <h2 className="mb-2 text-sm font-semibold">Markdown instructions</h2>
            {data.instructions.length === 0 && <p className="text-sm text-muted-foreground">No instruction files found.</p>}
            {data.instructions.map(({ file, text }) => (
              <ContextActions key={file.path} label={file.path} className="mb-2 rounded-lg border border-border bg-card p-3 text-sm" groups={[
                sourceActions(localPath(file.path), "instruction"),
                [{ label: "Copy instruction text", run: () => action.copy(text) },
                  { label: "Copy instruction path", run: () => action.copy(file.path) },
                  { label: "Copy instruction revision", run: () => action.copy(file.revision) }],
                draft && text.trim() ? [{ label: "Add saved instruction to conversation draft", run: () => {
                  const literal = text.replace(/@skill:[0-9a-f]{64}:[0-9a-f]{64}/g, (marker) => `\\${marker}`);
                  appendToDraft(`Saved instruction from ${file.path}\n\n${markdownExcerpt({ text: literal, language: "markdown" })}`);
                } }] : [],
              ]}>
                <details className="min-w-0 grow">
                  <summary className="cursor-pointer break-all font-mono text-xs">{file.path}</summary>
                  <pre className="mt-2 max-h-72 overflow-auto whitespace-pre-wrap break-words text-xs">{text}</pre>
                </details>
              </ContextActions>
            ))}
          </section>
          {draft && canBrowseSkills && <section aria-label="Compatible skills and plugin exports">
            <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
              <h2 className="text-sm font-semibold">Skills and plugin exports for {draft.title}</h2>
              <Button type="button" variant="outline" disabled={showCatalog && skills.isFetching} onClick={() => {
                if (showCatalog) void skills.refetch();
                else setCatalogRequest(catalogScope);
              }}>{showCatalog ? "Refresh compatible skills" : "Find compatible skills"}</Button>
            </div>
            {showCatalog && <>
              {skills.isPending && <p role="status" className="text-sm text-muted-foreground">Discovering compatible skills…</p>}
              {skills.isError && <p role="alert" className="text-sm text-red-300">{String(skills.error)}</p>}
              {skills.isSuccess && !skills.data.length && <p className="text-sm text-muted-foreground">No compatible skills for this conversation.</p>}
              {skills.data?.map((option) => {
                const { skill } = option;
                const label = skill.namespace ? `${skill.namespace} · ${skill.name}` : skill.name;
                return <ContextActions key={option.marker} label={label} className="mb-2 rounded-lg border border-border bg-card p-3 text-sm" groups={[
                  sourceActions(checkout ? skillCheckoutPath(checkout.checkoutRoot, skill.path) : undefined, "skill source"),
                  [{ label: "Copy skill path", run: () => action.copy(skill.path) },
                    { label: "Copy skill revision", run: () => action.copy(skill.revision) },
                    { label: "Copy skill ID", run: () => action.copy(skill.id) }],
                  [{ label: "Add skill to conversation draft", run: () => appendToDraft(option.marker, option) }],
                ]}>
                  <div className="min-w-0 grow"><strong>{label}</strong><p className="break-all font-mono text-xs text-muted-foreground">{skill.path}</p></div>
                </ContextActions>;
              })}
            </>}
          </section>}
        </div>
      )}
    </section>
  );
}
