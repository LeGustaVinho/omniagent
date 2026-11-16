import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";

type MarkdownRevision = { id: string; relativePath: string; sha256: string };
type RepositoryMarkdown = { revision: MarkdownRevision; markdown: string };

export function RepositoryMarkdownPicker({ checkoutId, onDeliver, delivering }: {
  checkoutId: string;
  onDeliver?: (revisionId: string) => void;
  delivering?: boolean;
}) {
  const client = useQueryClient();
  const [selected, setSelected] = useState("");
  const revisionsKey = ["repository-markdown-revisions", checkoutId] as const;
  const revisions = useQuery({
    queryKey: revisionsKey,
    queryFn: () => invoke<MarkdownRevision[]>("list_repository_markdown_revisions", { checkoutId }),
  });
  const revisionId = selected || revisions.data?.[0]?.id || "";
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
  const error = pick.error ?? revisions.error ?? document.error;
  return (
    <section className="pipeline-form pipeline-stack" aria-label="Repository Markdown source">
      <Button type="button" variant="outline" onClick={() => pick.mutate()} disabled={pick.isPending}>
        {pick.isPending ? "Selecting Markdown…" : "Select Markdown file"}
      </Button>
      {!!revisions.data?.length && <label>Saved source revision
        <select value={revisionId} onChange={(event) => setSelected(event.currentTarget.value)}>
          {revisions.data.map((revision) => (
            <option key={revision.id} value={revision.id}>{revision.relativePath} · {revision.sha256.slice(0, 12)}</option>
          ))}
        </select>
      </label>}
      {document.data && <details className="pipeline-evidence">
        <summary>Read saved Markdown</summary>
        <pre className="max-h-80 overflow-auto break-words">{document.data.markdown}</pre>
      </details>}
      {onDeliver && <Button type="button" onClick={() => onDeliver(revisionId)} disabled={!document.data || delivering}>
        {delivering ? "Starting delivery…" : "Start repository delivery"}
      </Button>}
      {error && <p role="alert" className="pipeline-note">{String(error)}</p>}
    </section>
  );
}
