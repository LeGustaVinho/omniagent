import { useEffect, useState } from "react";
import { renderNoteDiagram } from "@/knowledge-diagram";
import { downloadArtifact } from "@/download-artifact";

export function NoteDiagram({ source }: { source: string }) {
  const [rendered, setRendered] = useState<{ source: string; image?: string; error?: string }>();
  useEffect(() => {
    const request = new AbortController();
    void renderNoteDiagram(source, request.signal)
      .then((image) => { if (!request.signal.aborted) setRendered({ source, image }); })
      .catch(() => { if (!request.signal.aborted) setRendered({ source, error: "The Mermaid diagram could not be rendered. Review its source." }); });
    return () => request.abort();
  }, [source]);

  if (rendered?.source !== source) return <p role="status" className="file-help">Rendering diagram…</p>;
  return rendered.image ? <figure className="note-diagram"><img src={rendered.image} alt="Mermaid diagram from the note" /></figure>
    : <pre role="alert" className="note-preview-error">{rendered.error}{"\n"}{source}</pre>;
}

export async function downloadNoteDiagramPng(image: HTMLImageElement, name: string) {
  const { naturalWidth: width, naturalHeight: height } = image;
  if (!image.complete || width <= 0 || height <= 0) throw new Error("The diagram image is unavailable.");
  const scale = Math.min(2, 4096 / Math.max(width, height));
  const canvas = image.ownerDocument.createElement("canvas");
  canvas.width = Math.max(1, Math.round(width * scale));
  canvas.height = Math.max(1, Math.round(height * scale));
  const context = canvas.getContext("2d");
  if (!context) throw new Error("The diagram could not be exported.");
  context.drawImage(image, 0, 0, canvas.width, canvas.height);
  const blob = await new Promise<Blob>((resolve, reject) => canvas.toBlob(
    (result) => { if (result) resolve(result); else reject(new Error("The diagram could not be exported.")); }, "image/png",
  ));
  downloadArtifact(name, blob);
}
