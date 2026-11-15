import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { CustomizationEntry } from "@/bindings/CustomizationEntry";
import type { CustomizationInventory } from "@/bindings/CustomizationInventory";
import type { CustomizationKind } from "@/bindings/CustomizationKind";
import type { Provider } from "@/bindings/Provider";
import type { ReviewState } from "@/bindings/ReviewState";
import { Button } from "@/components/ui/button";
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

export function useCustomizationInventory(checkoutId: string | undefined) {
  return useQuery({
    queryKey: inventoryKey(checkoutId),
    queryFn: () => invoke<CustomizationInventory>("review_checkout_customizations", { checkoutId }),
    enabled: Boolean(checkoutId),
    staleTime: 15_000,
  });
}

export function CustomizationsPanel() {
  const checkoutId = useWorkspace().checkout?.checkoutId;
  const client = useQueryClient();
  const inventory = useCustomizationInventory(checkoutId);
  const approve = useMutation({
    mutationFn: ({ id, fingerprint }: CustomizationEntry) =>
      invoke<CustomizationInventory>("approve_checkout_customization", { checkoutId, entryId: id, fingerprint }),
    onSuccess: (result) => client.setQueryData(inventoryKey(checkoutId), result),
    onError: () => { void inventory.refetch(); },
  });
  const data = inventory.data;
  return (
    <section className="h-full overflow-auto p-6">
      <div className="mb-4 flex max-w-3xl items-center justify-between gap-3">
        <h1 className="text-xl font-semibold tracking-tight">Repository customizations</h1>
        <Button variant="outline" disabled={inventory.isFetching || approve.isPending}
          onClick={() => { approve.reset(); void inventory.refetch(); }}>Refresh</Button>
      </div>
      {inventory.isPending && <p role="status" className="text-sm text-muted-foreground">Reading repository files…</p>}
      {inventory.isError && <p role="alert" className="text-sm text-red-300">{String(inventory.error)}</p>}
      {approve.isError && <p role="alert" className="mb-3 text-sm text-red-300">{String(approve.error)}</p>}
      {data && (
        <div className="max-w-3xl space-y-6">
          <p className="break-all text-xs text-muted-foreground">{data.workingDirectory}</p>
          {data.unresolvedSources.length > 0 && (
            <details className="rounded-lg border border-border p-3 text-sm">
              <summary className="cursor-pointer font-medium">Sources that could not be inspected ({data.unresolvedSources.length})</summary>
              <ul className="mt-2 space-y-1 font-mono text-xs text-muted-foreground">
                {data.unresolvedSources.map((source) => <li key={source} className="break-all">{source}</li>)}
              </ul>
            </details>
          )}
          <section aria-label="Executable entries">
            <h2 className="mb-2 text-sm font-semibold">Executable entries</h2>
            {data.entries.length === 0 && <p className="text-sm text-muted-foreground">No executable declarations found.</p>}
            <div className="space-y-2">
              {data.entries.map((entry) => (
                <details key={entry.id} className="rounded-lg border border-border bg-card p-3 text-sm">
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
              ))}
            </div>
          </section>
          <section aria-label="Markdown instructions">
            <h2 className="mb-2 text-sm font-semibold">Markdown instructions</h2>
            {data.instructions.length === 0 && <p className="text-sm text-muted-foreground">No instruction files found.</p>}
            {data.instructions.map(({ file, text }) => (
              <details key={file.path} className="mb-2 rounded-lg border border-border bg-card p-3 text-sm">
                <summary className="cursor-pointer break-all font-mono text-xs">{file.path}</summary>
                <pre className="mt-2 max-h-72 overflow-auto whitespace-pre-wrap break-words text-xs">{text}</pre>
              </details>
            ))}
          </section>
        </div>
      )}
    </section>
  );
}
