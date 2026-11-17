import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { lazy, Suspense, useState } from "react";
import { Button } from "@/components/ui/button";
import { FileDialog, type FilePanelApi } from "@/ide-file-workspace";
import { lineEndingLabels, savedEncoding, sourceEncodingLabels, type FileFormat } from "@/source-file-format";

type HistoryEntry = { position: number; revisionId: string; recordedAt: number; origin: "captured" | "before_save" | "saved" };
type HistoryPage = { entries: HistoryEntry[]; nextBefore: number | null };
type HistoryText = FileFormat & { text: string };
type View = "current" | "previous" | "text";

const origins = { captured: "Previously captured", before_save: "Before saving", saved: "Saved" };
const timeFormat = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "medium" });
const MonacoComparison = lazy(() => import("@/git-diff-view").then(({ MonacoComparison }) => ({ default: MonacoComparison })));

function revisionText(checkoutId: string, path: string, position: number | undefined, enabled = true) {
  return {
    queryKey: ["checkout-document-history-text", checkoutId, path, position],
    queryFn: () => invoke<HistoryText>("checkout_document_history_text", { checkoutId, path, position }),
    enabled: enabled && position !== undefined,
    retry: false,
  };
}

export function CheckoutDocumentHistory({ checkoutId, path, editorText, readDisk, restore, ownerPanel, onClose }: {
  checkoutId: string;
  path: string;
  editorText?: string;
  readDisk: () => Promise<string>;
  restore?: (text: string) => Promise<void>;
  ownerPanel?: FilePanelApi;
  onClose: () => void;
}) {
  const [selected, select] = useState<number>();
  const [view, setView] = useState<View>("current");
  const [feedback, setFeedback] = useState<{ text: string; error?: boolean }>();
  const [restoring, setRestoring] = useState(false);
  const list = useInfiniteQuery({
    queryKey: ["checkout-document-history", checkoutId, path],
    initialPageParam: null as number | null,
    queryFn: ({ pageParam }) => invoke<HistoryPage>("checkout_document_history", { checkoutId, path, before: pageParam }),
    getNextPageParam: (last) => last.nextBefore,
    retry: false,
    refetchOnMount: "always",
  });
  const entries = list.data?.pages.flatMap((page) => page.entries) ?? [];
  const index = selected === undefined ? 0 : Math.max(0, entries.findIndex((entry) => entry.position === selected));
  const entry = entries[index];
  const previous = entries[index + 1];
  const revision = useQuery(revisionText(checkoutId, path, entry?.position));
  const earlier = useQuery(revisionText(checkoutId, path, previous?.position, view === "previous"));
  const disk = useQuery({
    queryKey: ["checkout-document-history-disk", checkoutId, path],
    queryFn: readDisk,
    enabled: Boolean(entry) && editorText === undefined && view === "current",
    retry: false,
    refetchOnMount: "always",
  });
  const against = view === "previous" ? earlier.data?.text : editorText ?? disk.data;
  const compareError = view === "previous" ? earlier.error : editorText === undefined ? disk.error : undefined;
  const shown = revision.data;

  async function copyText(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      setFeedback({ text: "Revision text copied" });
    } catch (failure) { setFeedback({ text: String(failure), error: true }); }
  }

  async function load(text: string) {
    if (!restore || restoring) return;
    setRestoring(true);
    try {
      await restore(text);
      onClose();
    } catch (failure) { setFeedback({ text: String(failure), error: true }); }
    finally { setRestoring(false); }
  }

  return <FileDialog title="Local file history" description={path} className="file-comparison-dialog" ownerPanel={ownerPanel} onClose={onClose}>
    <div className="file-comparison-body">
      {list.isPending && <p role="status">Loading local revisions…</p>}
      {list.error && <p role="alert">{String(list.error)}</p>}
      {list.isSuccess && !entry && <p>No local revisions recorded for this file yet.</p>}
      {entry && <>
        <div className="max-h-[34%] shrink-0 overflow-auto" aria-label="Local revisions">
          <ul className="grid gap-1">
            {entries.map((row) => {
              const at = new Date(row.recordedAt * 1000);
              return <li key={row.position}><button type="button" className="fw-btn w-full justify-start"
                aria-pressed={row.position === entry.position} title={row.revisionId}
                onClick={() => { select(row.position); setView("current"); setFeedback(undefined); }}>
                {origins[row.origin]} <time dateTime={at.toISOString()}>{timeFormat.format(at)}</time>
              </button></li>;
            })}
          </ul>
          {list.hasNextPage && <Button type="button" variant="ghost" disabled={list.isFetchingNextPage}
            onClick={() => void list.fetchNextPage()}>Load older revisions</Button>}
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <label className="flex items-center gap-2">View
            <select className="delivery-filter" aria-label="Revision comparison" value={view} onChange={(event) => setView(event.target.value as View)}>
              <option value="current">Compare with {editorText === undefined ? "file on disk" : "editor"}</option>
              {previous && <option value="previous">Compare with previous revision</option>}
              <option value="text">View revision text</option>
            </select>
          </label>
          {shown && <span className="text-xs text-muted-foreground">{sourceEncodingLabels[savedEncoding(shown)]} · {lineEndingLabels[shown.lineEndings]}</span>}
          <Button type="button" variant="outline" disabled={!shown} onClick={() => shown && void copyText(shown.text)}>Copy revision text</Button>
          {restore && <Button type="button" variant="outline" disabled={!shown || restoring}
            onClick={() => shown && void load(shown.text)}>Load text into editor</Button>}
        </div>
        {feedback && <p role={feedback.error ? "alert" : "status"}>{feedback.text}</p>}
        <div className="min-h-0 flex-1 overflow-auto" aria-label="Local revision preview">
          {revision.error ? <p role="alert">{String(revision.error)}</p> : !shown ? <p role="status">Loading revision text…</p>
            : view === "text" ? <pre className="whitespace-pre-wrap font-mono text-xs">{shown.text}</pre>
            : compareError ? <p role="alert">{String(compareError)}</p>
            : against === undefined ? <p role="status">Loading comparison…</p>
            : <Suspense fallback={<p role="status">Loading comparison…</p>}>
              <MonacoComparison layout="split" file={{ path, language: /\.(?:md|markdown)$/i.test(path) ? "markdown" : "plaintext",
                original: view === "previous" ? against : shown.text, modified: view === "previous" ? shown.text : against }}
                revisions={{ original: view === "previous" ? "Previous revision" : "Selected revision",
                  modified: view === "previous" ? "Selected revision" : editorText === undefined ? "File on disk" : "Editor" }} />
            </Suspense>}
        </div>
      </>}
    </div>
  </FileDialog>;
}
