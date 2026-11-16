import { useMutation } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";

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

function SavedAttachment({ attachment, conversationId, runId }: {
  attachment: AttachmentEvidence;
  conversationId: string;
  runId: string;
}) {
  const save = useMutation({
    mutationFn: async () => {
      const encoded = await invoke<string | null>("conversation_attachment_content", { conversationId, runId, index: attachment.ordinal });
      if (encoded === null) throw new Error("Saved attachment bytes are unavailable.");
      const bytes = Uint8Array.from(atob(encoded), (byte) => byte.charCodeAt(0));
      const url = URL.createObjectURL(new Blob([bytes], { type: attachment.mimeType }));
      const link = document.createElement("a");
      link.href = url;
      link.download = attachment.name;
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    },
  });
  const { transport } = attachment;
  return <li className="grid gap-1 border-t border-border pt-2 text-xs">
    <p className="font-medium">{attachment.name} · {transport.method === "native" ? "Native content" : transport.method === "excerpt" ? "Excerpt" : "File reference"}</p>
    <p className="break-all text-muted-foreground">Source {attachment.source.kind === "checkout" ? attachment.source.relativePath : attachment.source.path}</p>
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
    {save.error && <p role="alert">{String(save.error)}</p>}
  </li>;
}

export function SavedAttachments({ attachments, conversationId, runId }: {
  attachments: AttachmentEvidence[];
  conversationId: string;
  runId: string;
}) {
  if (!attachments.length) return null;
  return <details className="mt-2">
    <summary>Saved attachments</summary>
    <ol className="mt-2 grid gap-2" aria-label="Saved attachments">
      {attachments.map((attachment) => <SavedAttachment key={attachment.ordinal} attachment={attachment}
        conversationId={conversationId} runId={runId} />)}
    </ol>
  </details>;
}
