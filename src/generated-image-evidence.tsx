import { useMutation, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { Button } from "@/components/ui/button";

const captureIssues = {
  generation_not_completed: "The provider did not report a completed generation.",
  inline_image_unavailable: "The provider did not return inline image bytes.",
  size_limit_exceeded: "The image exceeds the 8 MiB preview limit.",
  dimension_limit_exceeded: "The image exceeds the preview dimensions limit.",
  invalid_inline_image: "The inline image could not be decoded.",
  unsupported_image_format: "Only PNG and JPEG can be previewed.",
};

export type GeneratedImageEvidence = {
  eventSequence: string;
  itemId: string;
  tool: string;
  producingModel: string | null;
  status: string | null;
  mime: "image/png" | "image/jpeg" | null;
  byteLen: number | null;
  sha256: string | null;
  unavailableReason: keyof typeof captureIssues | null;
};

type ImageScope = { projectId: string; conversationId: string; runId: string };

function GeneratedImage({ scope, image }: { scope: ImageScope; image: GeneratedImageEvidence }) {
  const [expanded, setExpanded] = useState(false);
  const [decoded, setDecoded] = useState(false);
  const args = { ...scope, eventSequence: image.eventSequence };
  const preview = useQuery({
    queryKey: ["conversation-generated-image", args],
    enabled: expanded && image.mime !== null,
    staleTime: Infinity,
    queryFn: () => invoke<string | null>("conversation_generated_image_evidence", args),
  });
  const qualification = useMutation({
    mutationFn: () => invoke<{ id: string; checkedAt: string }>("qualify_conversation_generated_image", args),
  });
  return <details className="grid rounded border border-border p-2" onToggle={(event) => setExpanded(event.currentTarget.open)}>
    <summary className="cursor-pointer">Generated image · Event {image.eventSequence}</summary>
    <dl className="break-all">
      <dt>Source</dt><dd>Codex App Server · {image.tool}</dd>
      <dt>Provider item</dt><dd>{image.itemId}</dd>
      <dt>Reported result</dt><dd>{image.status ?? "Unavailable"}</dd>
      {image.producingModel && <><dt>Reported producing model</dt><dd>{image.producingModel}</dd></>}
      {image.mime && <><dt>Image</dt><dd>{image.mime} · {image.byteLen?.toLocaleString()} bytes</dd></>}
      {image.sha256 && <><dt>SHA-256</dt><dd>{image.sha256}</dd></>}
    </dl>
    {image.unavailableReason && <p>{captureIssues[image.unavailableReason]}</p>}
    {expanded && image.mime && <>
      {preview.isPending && <p role="status">Loading generated image…</p>}
      {preview.error && <p role="alert">{String(preview.error)}</p>}
      {preview.data === null && <p>Saved image preview is unavailable.</p>}
      {preview.data && <img className="max-h-80 max-w-full object-contain" alt={`Image generated in event ${image.eventSequence}`}
        src={`data:${image.mime};base64,${preview.data}`} onLoad={() => setDecoded(true)} onError={() => setDecoded(false)} />}
      {preview.data && decoded && image.status === "completed" && !qualification.data && <Button type="button" variant="outline"
        disabled={qualification.isPending} onClick={() => qualification.mutate()}>
        {qualification.isPending ? "Saving qualification…" : "Qualify image generation for this profile"}
      </Button>}
      {qualification.error && <p role="alert">{String(qualification.error)}</p>}
      {qualification.data && <p role="status">Image generation verified for this saved profile and worktree. Evidence <code>{qualification.data.id}</code></p>}
    </>}
  </details>;
}

export function CapabilityOutputEvidence({ scope, generatedImages, managedValidation = false }: {
  scope: ImageScope;
  generatedImages: GeneratedImageEvidence[];
  managedValidation?: boolean;
}) {
  return <section className="grid gap-2" aria-label="Output evidence">
    <h3 className="font-medium">Output evidence</h3>
    {generatedImages.length === 0 && <p>No generated image evidence recorded for this run.</p>}
    {generatedImages.map((image) => <GeneratedImage key={image.eventSequence} scope={scope} image={image} />)}
    {managedValidation
      ? <p>Audio generation, browser control and Windows host control have separate managed validation fixtures.</p>
      : <>
        <p>Generated audio · Evidence capture unavailable</p>
        <p>Browser and host actions · Evidence capture unavailable</p>
      </>}
  </section>;
}
