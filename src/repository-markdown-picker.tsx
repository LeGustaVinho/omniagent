import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowRight, ChevronRight, FileText, FolderGit2, FolderOpen } from "lucide-react";
import { Button } from "@/components/ui/button";
import { PathText } from "@/components/ui/file-path";

type MarkdownRevision = { id: string; relativePath: string; sha256: string };
type RepositoryMarkdown = { revision: MarkdownRevision; markdown: string };

export function RepositoryMarkdownPicker({ checkoutId, pinnedRevisionId, onDeliver, delivering, launchDisabled, launchLabel = "Start repository delivery", deliveryWorktree }: {
  checkoutId: string;
  pinnedRevisionId?: string;
  onDeliver?: (revisionId: string) => void;
  delivering?: boolean;
  launchDisabled?: boolean;
  launchLabel?: string;
  deliveryWorktree?: string;
}) {
  const client = useQueryClient();
  const [selected, setSelected] = useState("");
  const pinned = pinnedRevisionId !== undefined;
  const revisionsKey = ["repository-markdown-revisions", checkoutId] as const;
  const revisions = useQuery({
    queryKey: revisionsKey,
    queryFn: () => invoke<MarkdownRevision[]>("list_repository_markdown_revisions", { checkoutId }),
    enabled: !pinned,
  });
  const revisionId = pinnedRevisionId ?? (selected || revisions.data?.[0]?.id || "");
  const document = useQuery({
    queryKey: ["repository-markdown", checkoutId, revisionId],
    queryFn: () => invoke<RepositoryMarkdown | null>("read_repository_markdown", { checkoutId, revisionId }),
    enabled: Boolean(revisionId),
  });
  const pick = useMutation({
    mutationFn: () => invoke<RepositoryMarkdown | null>("choose_repository_markdown", { checkoutId }),
    onSuccess: (file) => {
      if (!file) return;
      client.setQueryData(["repository-markdown", checkoutId, file.revision.id], file);
      client.setQueryData<MarkdownRevision[]>(revisionsKey, (current) => [
        file.revision,
        ...(current ?? []).filter((revision) => revision.id !== file.revision.id),
      ]);
      setSelected(file.revision.id);
      void client.invalidateQueries({ queryKey: revisionsKey });
    },
  });
  const error = (pinned ? undefined : pick.error ?? revisions.error) ?? document.error;
  const delivery = deliveryWorktree !== undefined;
  const filePicker = !pinned && <Button type="button" variant="outline" className={delivery ? "repo-delivery-file-picker" : undefined}
    onClick={() => pick.mutate()} disabled={pick.isPending || delivering}>
    {delivery && <FolderOpen size={15} aria-hidden />}
    {pick.isPending ? "Selecting Markdown…" : "Select Markdown file"}
  </Button>;
  const launch = onDeliver && <Button type="button" aria-busy={delivering || undefined} onClick={() => onDeliver(revisionId)}
    disabled={!document.data || delivering || launchDisabled || pick.isPending}>
    {delivering ? "Starting…" : launchLabel}
    {delivery && <ArrowRight size={16} aria-hidden />}
  </Button>;
  return (
    <section className={`pipeline-form pipeline-stack${delivery ? " repo-delivery-card repo-delivery-source" : ""}`} aria-label="Repository Markdown source">
      {delivery && <header className="repo-delivery-card-heading">
        <span className="repo-delivery-card-icon"><FileText size={17} aria-hidden /></span>
        <div><h3>Markdown brief</h3><p className="repo-delivery-worktree" title={deliveryWorktree}><FolderGit2 size={14} aria-hidden /><PathText path={deliveryWorktree} /></p></div>
      </header>}
      {!delivery && filePicker}
      {!pinned && !!revisions.data?.length && <label>Saved source revision
        <select value={revisionId} disabled={delivery && delivering} onChange={(event) => setSelected(event.currentTarget.value)}>
          {revisions.data.map((revision) => (
            <option key={revision.id} value={revision.id}>{revision.relativePath} · {revision.sha256.slice(0, 12)}</option>
          ))}
        </select>
      </label>}
      {delivery && filePicker}
      {delivery && !pinned && !revisionId && (revisions.isPending
        ? <p className="repo-delivery-hint" role="status">Loading saved revisions…</p>
        : revisions.isSuccess && <p className="repo-delivery-hint">No saved Markdown revisions in this worktree.</p>)}
      {delivery && !pinned && revisionId && document.isPending && <p className="repo-delivery-hint" role="status">Loading saved Markdown…</p>}
      {delivery && !pinned && document.data === null && <p role="alert">This saved Markdown revision is unavailable.</p>}
      {pinned && document.isPending && <p role="status">Loading the saved note…</p>}
      {pinned && document.data === null && <p role="alert">This saved note revision is unavailable.</p>}
      {pinned && document.data && <p className="pipeline-meta break-all">
        {document.data.revision.relativePath} · {document.data.revision.sha256.slice(0, 12)}
      </p>}
      {document.data && <details className={delivery ? "pipeline-evidence repo-delivery-document" : "pipeline-evidence"}>
        <summary>{delivery && <ChevronRight size={15} aria-hidden />}Read saved Markdown</summary>
        {delivery && <p className="repo-delivery-hint">SHA-256 <code>{document.data.revision.sha256}</code></p>}
        <pre className="max-h-80 overflow-auto break-words">{document.data.markdown}</pre>
      </details>}
      {delivery && launch ? <div className="repo-delivery-source-actions">
        {launch}<p className="repo-delivery-hint">After starting, continue in the run’s Planning section.</p>
      </div> : launch}
      {error && <p role="alert" className="pipeline-note">{String(error)}</p>}
    </section>
  );
}
