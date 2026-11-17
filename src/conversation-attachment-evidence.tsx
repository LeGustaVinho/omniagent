import { useMutation } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { ContextActions } from "@/components/ui/context-actions";
import { useAttachmentActions } from "@/conversation-attachment-actions";
import { decodeArtifactBytes, downloadArtifact } from "@/download-artifact";

export type AttachmentEvidence = {
  ordinal: number;
  name: string;
  mimeType: string;
  source: { kind: "checkout"; relativePath: string } | { kind: "selected_file"; path: string };
  sourceSha256: string;
  sourceSize: number;
  messageRevisionId: string;
  reportedReads: { eventSequence: string; path: string; tool: string }[];
  transport: { method: "native"; encoding: string; runnerPath: string | null; resourceUri: string | null }
    | { method: "excerpt"; start: number; end: number; text: string }
    | { method: "reference"; runnerPath: string; uri: string | null };
};

function SavedAttachment({ attachment, conversationId, runId, checkoutId }: {
  attachment: AttachmentEvidence;
  conversationId: string;
  runId: string;
  checkoutId?: string;
}) {
  const save = useMutation({
    mutationFn: async () => {
      const encoded = await invoke<string | null>("conversation_attachment_content", { conversationId, runId, index: attachment.ordinal });
      if (encoded === null) throw new Error("Saved attachment bytes are unavailable.");
      downloadArtifact(attachment.name, new Blob([decodeArtifactBytes(encoded)], { type: attachment.mimeType }));
    },
  });
  const { transport } = attachment;
  const { path, checkout, clipboard, copy, feedback } = useAttachmentActions(attachment.source, attachment.name, checkoutId);
  const runnerPath = transport.method === "excerpt" ? null : transport.runnerPath;
  const uri = transport.method === "native" ? transport.resourceUri : transport.method === "reference" ? transport.uri : null;
  const title = `${attachment.name} · ${transport.method === "native" ? "Native content" : transport.method === "excerpt" ? "Excerpt" : "File reference"}`;
  return <li className="grid gap-1 border-t border-border pt-2 text-xs">
    <ContextActions label={title} groups={[checkout, transport.method === "native" ? [
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
  </li>;
}

export function SavedAttachments({ attachments, conversationId, runId, checkoutId }: {
  attachments: AttachmentEvidence[];
  conversationId: string;
  runId: string;
  checkoutId?: string;
}) {
  if (!attachments.length) return null;
  return <details className="mt-2">
    <summary>Saved attachments</summary>
    <ol className="mt-2 grid gap-2" aria-label="Saved attachments">
      {attachments.map((attachment) => <SavedAttachment key={attachment.ordinal} attachment={attachment}
        conversationId={conversationId} runId={runId} checkoutId={checkoutId} />)}
    </ol>
  </details>;
}
