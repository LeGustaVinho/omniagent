import { useMutation, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { useCheckoutConversationActions } from "@/checkout-conversation-actions";
import { Button } from "@/components/ui/button";
import { ContextActions } from "@/components/ui/context-actions";
import { useAttachmentActions } from "@/conversation-attachment-actions";
import type { AttachmentSource } from "@/conversation-attachments";
import { decodeArtifactBytes, downloadArtifact } from "@/download-artifact";
import { FileDialog } from "@/ide-file-workspace";
import { isNoteMediaType, SafeAttachmentMedia } from "@/repository-note-media";

export type AttachmentEvidence = {
  ordinal: number;
  name: string;
  mimeType: string;
  source: AttachmentSource;
  sourceSha256: string;
  sourceSize: number;
  messageRevisionId: string;
  reportedReads: { eventSequence: string; path: string; tool: string }[];
  transport: { method: "native"; encoding: string; runnerPath: string | null; resourceUri: string | null }
    | { method: "excerpt"; start: number; end: number; text: string }
    | { method: "reference"; runnerPath: string; uri: string | null };
};

export type AttachmentDraftDestination = { checkoutId: string; open: (conversationId: string) => void };

async function readSavedBytes(conversationId: string, runId: string, index: number) {
  const encoded = await invoke<string | null>("conversation_attachment_content", { conversationId, runId, index });
  if (encoded === null) throw new Error("Saved attachment bytes are unavailable.");
  return encoded;
}

function SavedAttachment({ attachment, conversationId, runId, checkoutId, reuse }: {
  attachment: AttachmentEvidence;
  conversationId: string;
  runId: string;
  checkoutId?: string;
  reuse?: (attachment: AttachmentEvidence) => void;
}) {
  const [preview, setPreview] = useState(false);
  const save = useMutation({
    mutationFn: async () => {
      const encoded = await readSavedBytes(conversationId, runId, attachment.ordinal);
      downloadArtifact(attachment.name, new Blob([decodeArtifactBytes(encoded)], { type: attachment.mimeType }));
    },
  });
  const { transport } = attachment;
  const { path, checkout, clipboard, copy, feedback } = useAttachmentActions(attachment.source, attachment.name, checkoutId);
  const runnerPath = transport.method === "excerpt" ? null : transport.runnerPath;
  const uri = transport.method === "native" ? transport.resourceUri : transport.method === "reference" ? transport.uri : null;
  const title = `${attachment.name} · ${transport.method === "native" ? "Native content" : transport.method === "excerpt" ? "Excerpt" : "File reference"}`;
  const reuseLabel = transport.method === "native" ? "Reuse saved attachment…"
    : transport.method === "excerpt" ? "Add saved excerpt to draft…" : "Add saved reference to draft…";
  const previewable = transport.method === "native" && attachment.sourceSize <= 16 * 1024 * 1024
    && (attachment.mimeType === "text/plain" || isNoteMediaType(attachment.mimeType));
  return <li className="grid gap-1 border-t border-border pt-2 text-xs">
    <ContextActions label={title} groups={[reuse ? [{ label: reuseLabel, run: () => reuse(attachment) }] : [], checkout, transport.method === "native" ? [
      ...(previewable ? [{ label: "Preview saved attachment", run: () => setPreview(true) }] : []),
      { label: "Download saved bytes", disabled: save.isPending, run: () => save.mutate() },
    ] : [], [...clipboard,
      { label: "Copy saved SHA-256", run: () => copy(attachment.sourceSha256) },
      { label: "Copy message revision ID", run: () => copy(attachment.messageRevisionId) },
      ...(transport.method === "excerpt" ? [{ label: "Copy saved excerpt", run: () => copy(transport.text) }] : []),
      ...(runnerPath ? [{ label: "Copy runner path", run: () => copy(runnerPath) }] : []),
      ...(uri ? [{ label: "Copy resource URI", run: () => copy(uri) }] : []),
    ]]}>
      <p className="font-medium">{title}</p>
    </ContextActions>
    <p className="break-all text-muted-foreground">Source {path}</p>
    <p className="break-all text-muted-foreground">Message revision {attachment.messageRevisionId}</p>
    <p className="break-all text-muted-foreground">SHA-256 {attachment.sourceSha256} · {attachment.sourceSize.toLocaleString()} bytes</p>
    {transport.method === "excerpt" ? <>
      <p>UTF-8 bytes {transport.start}–{transport.end}</p>
      <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words">{transport.text}</pre>
    </> : <>
      {transport.runnerPath && <p className="break-all">Runner path {transport.runnerPath}</p>}
      {transport.method === "reference" && transport.uri && <p className="break-all">URI {transport.uri}</p>}
      {transport.method === "native" && <>
        <p>{transport.encoding.replaceAll("_", " ")}</p>
        {transport.resourceUri && <p className="break-all">URI {transport.resourceUri}</p>}
        <Button type="button" variant="outline" disabled={save.isPending} onClick={() => save.mutate()}>Download saved bytes</Button>
      </>}
    </>}
    {attachment.reportedReads.length > 0 && <ol aria-label="CLI-reported reads" className="grid gap-1">
      {attachment.reportedReads.map(({ eventSequence, path, tool }) => <li key={eventSequence} className="break-all text-muted-foreground">
        CLI reported read via {tool} · {path} · Event {eventSequence}
      </li>)}
    </ol>}
    {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
    {save.error && <p role="alert">{String(save.error)}</p>}
    {preview && <SavedAttachmentPreview attachment={attachment} conversationId={conversationId} runId={runId} close={() => setPreview(false)} />}
  </li>;
}

function SavedAttachmentPreview({ attachment, conversationId, runId, close }: {
  attachment: AttachmentEvidence; conversationId: string; runId: string; close: () => void;
}) {
  const content = useQuery({ queryKey: ["saved-attachment-preview", conversationId, runId, attachment.ordinal, attachment.messageRevisionId],
    queryFn: () => readSavedBytes(conversationId, runId, attachment.ordinal), retry: false, gcTime: 0 });
  return <FileDialog title="Saved attachment preview" description={`${attachment.name} · Message revision ${attachment.messageRevisionId}`} onClose={close}>
    {content.isPending && <p role="status">Loading saved bytes…</p>}
    {content.error && <p role="alert">{String(content.error)}</p>}
    {content.data !== undefined && <SafeAttachmentMedia base64={content.data} mimeType={attachment.mimeType} title={attachment.name} />}
  </FileDialog>;
}

export function SavedAttachments({ attachments, conversationId, runId, checkoutId, draftDestination }: {
  attachments: AttachmentEvidence[];
  conversationId: string;
  runId: string;
  checkoutId?: string;
  draftDestination?: AttachmentDraftDestination;
}) {
  const draft = useCheckoutConversationActions(draftDestination?.checkoutId ?? "", !draftDestination, (id) => draftDestination?.open(id));
  function reuse(attachment: AttachmentEvidence) {
    const { name, ordinal, sourceSha256, messageRevisionId, transport } = attachment;
    const reference = `${name} · Conversation ${conversationId} · Run ${runId} · Attachment ${ordinal + 1} · Message revision ${messageRevisionId} · SHA-256 ${sourceSha256}`;
    if (transport.method === "native") {
      draft.startRecordedFile({ title: name, reference, sourceConversationId: conversationId, runId, ordinal, messageRevisionId, sourceSha256 });
      return;
    }
    const details = transport.method === "excerpt" ? `UTF-8 bytes ${transport.start}–${transport.end}` : "Recorded file reference";
    const text = transport.method === "excerpt" ? transport.text
      : [`Recorded runner path: ${transport.runnerPath}`, transport.uri && `Recorded URI: ${transport.uri}`].filter(Boolean).join("\n");
    draft.startContextText({ title: name, reference: `${reference} · ${details}`, text, confirmSource: async () => {
      const current = await invoke<{ attachments: AttachmentEvidence[] } | null>("conversation_input", { conversationId, runId });
      const saved = current?.attachments.find((item) => item.ordinal === ordinal && item.messageRevisionId === messageRevisionId
        && item.sourceSha256 === sourceSha256);
      if (!saved || JSON.stringify(saved.transport) !== JSON.stringify(transport)) {
        throw new Error("This saved attachment changed or was removed. Open its recorded input again.");
      }
    } });
  }
  if (!attachments.length) return null;
  return <>
    <details className="mt-2">
      <summary>Saved attachments</summary>
      <ol className="mt-2 grid gap-2" aria-label="Saved attachments">
        {attachments.map((attachment) => <SavedAttachment key={attachment.ordinal} attachment={attachment}
          conversationId={conversationId} runId={runId} checkoutId={checkoutId} reuse={draftDestination ? reuse : undefined} />)}
      </ol>
    </details>
    {draft.dialog}
  </>;
}
