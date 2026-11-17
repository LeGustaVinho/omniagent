import { invoke } from "@tauri-apps/api/core";
import { X } from "lucide-react";
import { Dialog } from "radix-ui";
import { useEffect, useState } from "react";
import { useFileEditorDocument } from "@/file-editor-popout";

export type GitLineBlameRequest = { checkoutId: string; path: string; line: number; editorText: string };

type GitLineBlame = { status: "committed"; head: string; commit: string; author: string; authoredAt: number; summary: string;
  headLine: number; originalLine: number }
  | { status: "uncommitted" | "unavailable"; reason: string };

export function GitLineBlameDialog({ request, close, restoreFocus, copy }: {
  request: GitLineBlameRequest;
  close: () => void;
  restoreFocus: () => void;
  copy?: (text: string, status: string) => void;
}) {
  const owner = useFileEditorDocument();
  const [lookup, setLookup] = useState<{ request: GitLineBlameRequest; result?: GitLineBlame; error?: string }>();
  useEffect(() => {
    let active = true;
    void invoke<GitLineBlame>("git_checkout_line_blame", request).then(
      (result) => { if (active) setLookup({ request, result }); },
      (error: unknown) => { if (active) setLookup({ request, error: String(error) }); },
    );
    return () => { active = false; };
  }, [request]);
  const state = lookup?.request === request ? lookup : undefined;
  const result = state?.result;
  const date = result?.status === "committed" ? new Date(result.authoredAt * 1000) : undefined;

  return <Dialog.Root open modal={!owner || owner === document} onOpenChange={(open) => { if (!open) close(); }}>
    <Dialog.Portal container={owner?.body ?? document.body}>
      <Dialog.Overlay className="file-dialog-overlay" />
      <Dialog.Content className="file-dialog" onCloseAutoFocus={(event) => { event.preventDefault(); restoreFocus(); }}>
        <header>
          <Dialog.Title>Git line authorship</Dialog.Title>
          <Dialog.Close type="button" aria-label="Close dialog"><X size={17} /></Dialog.Close>
        </header>
        <Dialog.Description>{request.path}:{request.line}</Dialog.Description>
        {!result && !state?.error && <p role="status">Reading Git line authorship…</p>}
        {state?.error && <p role="alert">{state.error}</p>}
        {result && result.status !== "committed" && <p role="status">{result.reason}</p>}
        {result?.status === "committed" && <>
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
            <dt>Author</dt><dd className="break-all">{result.author || "Unknown"}</dd>
            {date && !Number.isNaN(date.valueOf()) && <><dt>Authored</dt><dd><time dateTime={date.toISOString()}>{date.toLocaleString()}</time></dd></>}
            <dt>Commit</dt><dd><code title={result.commit}>{result.commit.slice(0, 12)}</code></dd>
            {result.summary && <><dt>Summary</dt><dd className="break-all">{result.summary}</dd></>}
            <dt>Checkout HEAD</dt><dd><code title={result.head}>{result.head.slice(0, 12)}</code> · line {result.headLine}</dd>
            {result.originalLine !== result.headLine && <><dt>Line in authoring commit</dt><dd>{result.originalLine}</dd></>}
          </dl>
          {copy && <div className="file-dialog-footer"><button type="button" className="fw-btn fw-outlined"
            onClick={() => copy(result.commit, "Copied commit ID")}>Copy commit ID</button></div>}
        </>}
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
