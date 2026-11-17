import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { useCheckoutConversationActions } from "@/checkout-conversation-actions";
import { ContextActions } from "@/components/ui/context-actions";
import type { CheckoutReview } from "@/git-checkout-changes";
import { MonacoComparison } from "@/git-diff-view";
import { compareGitFileVersions, gitFilePermalink, gitFileText } from "@/git-file-versions";
import { useGitReviewComments } from "@/git-review-comments";
import { GitWorkspaceTable } from "@/git-workspace-frame";
import { InlineIssue } from "@/tasks-git-ui";

type Entry = { sha: string; parent: string | null; author: string; committedAt: string; subject: string;
  path: string; previousPath: string | null; status: string };
type FileHistory = { head: string | null; entries: Entry[]; hasMore: boolean };
type Page = { pinnedHead: string | null; offset: number };
type View = "parent" | "disk" | "index" | "head" | "contents" | "selected";
const viewLabels: Record<View, string> = {
  parent: "Compare with parent", disk: "Compare with file on disk", index: "Compare with staged index",
  head: "Compare with current HEAD", contents: "View committed file", selected: "Compare with selected revision",
};
const labels: Record<string, string> = { A: "Added", C: "Copied", D: "Deleted", M: "Modified", R: "Renamed", T: "Type changed" };

const version = (entry: Entry) => ({ kind: "commit" as const, sha: entry.sha, path: entry.path });
const short = (sha: string) => sha.slice(0, 12);

function comparisonFor(entry: Entry, view: View, reference?: Entry) {
  const selected = version(entry);
  switch (view) {
    case "parent": return {
      base: entry.status === "A" || !entry.parent ? null : { kind: "commit" as const, sha: entry.parent, path: entry.previousPath ?? entry.path },
      revised: selected,
      ranges: [entry.status !== "A" && entry.parent ? `${short(entry.parent)} · ${entry.previousPath ?? entry.path}` : "Before the file was added",
        `${short(entry.sha)} · ${entry.path}`],
    };
    case "disk": return { base: selected, revised: { kind: "worktree" as const }, ranges: [short(entry.sha), "File on disk"] };
    case "index": return { base: selected, revised: { kind: "index" as const }, ranges: [short(entry.sha), "Staged index"] };
    case "head": return { base: selected, revised: { kind: "head" as const }, ranges: [short(entry.sha), "Current HEAD"] };
    case "contents": return { base: null, revised: selected, ranges: ["", `${short(entry.sha)} · ${entry.path}`] };
    case "selected": return reference && { base: version(reference), revised: selected,
      ranges: [`${short(reference.sha)} · ${reference.path}`, `${short(entry.sha)} · ${entry.path}`] };
  }
}

export function GitFileHistoryView({ review, path, openFile, openConversation, clear }: { review: CheckoutReview; path: string;
  openFile?: () => void; openConversation?: (id: string) => void; clear: () => void }) {
  const { checkoutId } = review;
  const [branch, setBranch] = useState("");
  const [picked, select] = useState<{ sha: string; view: View }>();
  const [reference, setReference] = useState<Entry>();
  const [layout, setLayout] = useState<"auto" | "split" | "unified">("auto");
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const head = branch ? review.branches.find((item) => item.reference === branch)?.sha : review.revision.head;
  const history = useInfiniteQuery({
    queryKey: ["git-file-history", checkoutId, path, branch, head],
    initialPageParam: { pinnedHead: null, offset: 0 } as Page,
    queryFn: ({ pageParam }) => invoke<FileHistory>("git_checkout_file_history", { checkoutId, path, branch: branch || null, ...pageParam }),
    getNextPageParam: (last, pages): Page | undefined => last.hasMore && last.head
      ? { pinnedHead: last.head, offset: pages.reduce((count, page) => count + page.entries.length, 0) } : undefined,
    retry: false,
    refetchOnWindowFocus: false,
  });
  const entries = history.data?.pages.flatMap((page) => page.entries) ?? [];
  const entry = entries.find(({ sha }) => sha === picked?.sha) ?? entries[0];
  const view = picked?.sha === entry?.sha ? picked?.view ?? "parent" : "parent";
  const comparison = useMemo(() => entry ? comparisonFor(entry, view, reference) : undefined, [entry, view, reference]);
  const load = useQuery({
    queryKey: ["git-file-versions", checkoutId, path, comparison,
      view === "head" ? review.revision.head : view === "index" ? review.revision.index : null],
    queryFn: () => compareGitFileVersions(checkoutId, path, comparison!.base, comparison!.revised),
    enabled: Boolean(comparison), retry: false,
  });
  const sides = useMemo(() => {
    if (!load.data || !entry) return undefined;
    const { original, modified } = load.data;
    if (original.status === "unavailable" || modified.status === "unavailable") return {
      error: [original, modified].flatMap((side) => side.status === "unavailable" ? [side.reason] : []).join(" "),
    };
    const originalPath = comparison?.base?.kind === "commit" ? comparison.base.path : path;
    return { file: { path: entry.path, originalPath, language: /\.(md|markdown)$/i.test(entry.path) ? "markdown" : "plaintext",
      original: gitFileText(original), modified: gitFileText(modified) }, empty: original.status === "missing" && modified.status === "missing" };
  }, [load.data, entry, comparison, path]);
  const conversation = useCheckoutConversationActions(checkoutId, !openConversation, (id) => openConversation?.(id));
  const comments = useGitReviewComments({
    checkoutId, surface: "versions", matchPath: path, comparisonPath: path, language: sides?.file?.language,
    scope: view !== "contents" && sides?.file && !load.error ? load.data?.commentScope : undefined,
    startContextText: openConversation ? conversation.startContextText : undefined,
  });
  const [reviewLocation, setReviewLocation] = useState(comments.annotations.location);
  if (reviewLocation !== comments.annotations.location) {
    setReviewLocation(comments.annotations.location);
    if (comments.annotations.location?.side === "original") setLayout("split");
  }

  async function copy(read: () => string | Promise<string>, message: string) {
    setError("");
    setStatus("");
    try { await navigator.clipboard.writeText(await read()); setStatus(message); }
    catch (failure) { setError(String(failure)); }
  }

  return <section className="flex min-h-0 flex-1 flex-col" aria-label="File history">
    <div className="delivery-toolbar delivery-filters">
      <h3 className="min-w-0 wrap-anywhere font-mono text-sm" title={path}>{path}</h3>
      <label className="delivery-field">History branch
        <select className="delivery-filter" value={branch} onChange={(event) => { setBranch(event.currentTarget.value); select(undefined); setReference(undefined); }}>
          <option value="">Current checkout HEAD</option>
          {review.branches.map(({ name, reference }) => <option key={reference} value={reference}>{name}</option>)}
        </select>
      </label>
      {openFile && <Button type="button" variant="outline" onClick={openFile}>Open working file</Button>}
      <Button type="button" variant="ghost" onClick={clear}>All commits</Button>
    </div>
    <div className="max-h-[48%] shrink-0 overflow-auto border-b border-border px-3 py-2">
      {history.isPending && <p role="status" className="text-sm">Loading file history…</p>}
      {(history.error || error) && <InlineIssue>{history.error ? String(history.error) : error}</InlineIssue>}
      {status && <p role="status" className="text-xs">{status}</p>}
      {history.data && entries.length === 0 && <p className="text-sm text-muted-foreground">This path has no committed history on the selected branch.</p>}
      {entries.length > 0 && <GitWorkspaceTable label="File revisions" columns={["Commit", "Change", "Author", "Recorded"]}>
        {entries.map((row) => <tr key={row.sha}>
          <td><ContextActions label={`${row.subject || "Commit"} ${short(row.sha)}`} groups={[[
            ...(Object.entries(viewLabels) as [View, string][]).filter(([mode]) =>
              (mode !== "contents" || row.status !== "D") && (mode !== "selected" || reference && reference.sha !== row.sha))
              .map(([mode, label]) => ({ label, run: () => select({ sha: row.sha, view: mode }) })),
            { label: reference?.sha === row.sha ? "Clear comparison base" : "Select revision as comparison base",
              run: () => setReference(reference?.sha === row.sha ? undefined : row) },
          ], [
            { label: "Copy full commit SHA", run: () => void copy(() => row.sha, "Copied commit SHA") },
            { label: "Copy path at this revision", run: () => void copy(() => row.path, "Copied revision path") },
            ...(row.status === "D" ? [] : [{ label: "Copy committed permalink", run: () => void copy(
              () => gitFilePermalink(checkoutId, row.path, row.sha), "Copied committed permalink",
            ) }]),
            ...(row.status === "D" ? [] : [{ label: "Copy committed text", run: () => void copy(async () => {
              const snapshot = await compareGitFileVersions(checkoutId, path, null, version(row));
              return gitFileText(snapshot.modified);
            }, "Copied committed text") }]),
          ]]}>
            <button type="button" className="grid min-w-0 gap-1 text-left hover:underline" aria-pressed={entry?.sha === row.sha}
              onClick={() => select({ sha: row.sha, view: "parent" })}>
              <span className="break-words">{row.subject || "Commit"}</span><code className="text-xs text-muted-foreground">{short(row.sha)}</code>
            </button>
          </ContextActions></td>
          <td><span>{labels[row.status] ?? row.status}</span>{(row.previousPath || row.path !== path) &&
            <code className="block wrap-anywhere text-xs text-muted-foreground">{row.previousPath && `${row.previousPath} → `}{row.path}</code>}</td>
          <td>{row.author}</td><td className="whitespace-nowrap"><time dateTime={row.committedAt}>{row.committedAt}</time></td>
        </tr>)}
      </GitWorkspaceTable>}
      {history.hasNextPage && <Button className="mt-2" type="button" variant="outline" disabled={history.isFetchingNextPage}
        onClick={() => void history.fetchNextPage()}>{history.isFetchingNextPage ? "Loading older revisions…" : "Load older revisions"}</Button>}
    </div>
    {comments.panel}
    {comparison && <div className="flex min-h-0 flex-1 flex-col" aria-label="File revision comparison">
      <div className="delivery-toolbar delivery-filters">
        <span className="wrap-anywhere text-xs" aria-label="File revisions in comparison">{comparison.ranges.filter(Boolean).join(" → ")}</span>
        {view === "disk" && <Button variant="ghost" type="button" disabled={load.isFetching} onClick={() => void load.refetch()}>
          Refresh disk version
        </Button>}
        {reference && <Button variant="ghost" type="button" onClick={() => { setReference(undefined); if (view === "selected") select(undefined); }}>
          Clear base {short(reference.sha)}
        </Button>}
        {view !== "contents" && <label className="delivery-field ml-auto">Diff layout
          <select className="delivery-filter" value={layout} onChange={(event) => setLayout(event.currentTarget.value as typeof layout)}>
            <option value="auto">Auto</option><option value="split">Split</option><option value="unified">Unified</option>
          </select>
        </label>}
      </div>
      <div className="min-h-0 flex-1 overflow-auto" aria-live="polite">
        {load.isPending ? <p role="status" className="p-3 text-sm">Loading file versions…</p>
          : load.error || sides?.error ? <div className="p-3"><InlineIssue>{load.error ? String(load.error) : sides?.error}</InlineIssue></div>
            : sides?.empty ? <p className="p-3 text-sm">This file is absent from both revisions.</p>
              : sides?.file && (view === "contents" ? <pre className="p-3 font-mono text-xs">{sides.file.modified}</pre>
                : <MonacoComparison file={sides.file} layout={layout} review={comments.annotations}
                  revisions={{ original: comparison.ranges[0]!, modified: comparison.ranges[1]! }} />)}
      </div>
    </div>}
    {comments.dialog}
    {conversation.dialog}
  </section>;
}
