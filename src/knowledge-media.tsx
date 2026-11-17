import { useEffect, useRef, useState } from "react";
import pdfWorkerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
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

export const noteAttachments = [
  { name: "release-evidence.pdf", kind: "pdf", detail: "Readiness report · 1 page" },
  { name: "rollout-diagram.png", kind: "image", detail: "Review sequence · image" },
  { name: "rollout-tone.wav", kind: "audio", detail: "Release cue · 0.4 s" },
  { name: "setup-walkthrough.webm", kind: "video", detail: "Recovery walkthrough · sample clip" },
  { name: "archived-log.pdf", kind: "missing", detail: "File unavailable in this checkout" },
] as const;

export type NoteAttachment = (typeof noteAttachments)[number];

function standardPdf() {
  const stream = "BT /F1 22 Tf 56 720 Td (Release evidence) Tj /F1 13 Tf 0 -36 Td (Startup checks completed.) Tj 0 -22 Td (Candidate: Readiness checklist) Tj ET";
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
  ];
  let output = "%PDF-1.4\n";
  const offsets = objects.map((body, index) => {
    const offset = output.length;
    output += `${index + 1} 0 obj\n${body}\nendobj\n`;
    return offset;
  });
  const xref = output.length;
  output += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n` + offsets.map((offset) => `${String(offset).padStart(10, "0")} 00000 n \n`).join("");
  output += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return new TextEncoder().encode(output);
}

function PdfPreview() {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [status, setStatus] = useState("Rendering PDF…");
  useEffect(() => {
    const target = canvas.current;
    if (!target) return;
    let current = true;
    let document: import("pdfjs-dist").PDFDocumentLoadingTask | undefined;
    let render: import("pdfjs-dist").RenderTask | undefined;

    void (async () => {
      try {
        const pdfjs = await import("pdfjs-dist");
        if (!current) return;
        pdfjs.GlobalWorkerOptions.workerSrc = pdfWorkerUrl;
        document = pdfjs.getDocument({ data: standardPdf(), useSystemFonts: true });
        const pdf = await document.promise;
        const page = await pdf.getPage(1);
        if (!current) return;
        const viewport = page.getViewport({ scale: 0.85 });
        target.width = viewport.width;
        target.height = viewport.height;
        render = page.render({ canvas: target, viewport, background: "#ffffff" });
        await render.promise;
        if (current) setStatus("");
      } catch {
        if (current) setStatus("The sample PDF could not be rendered.");
      }
    })();
    return () => { current = false; render?.cancel(); if (document) void document.destroy().catch(() => undefined); };
  }, []);
  return <><canvas ref={canvas} className="note-pdf" aria-label="Page one of the release evidence PDF" />{status && <p role="status">{status}</p>}</>;
}

function audioSource() {
  const sampleRate = 8000;
  const samples = sampleRate * 0.4;
  const bytes = new Uint8Array(44 + samples);
  const data = new DataView(bytes.buffer);
  bytes.set(new TextEncoder().encode("RIFF"), 0);
  data.setUint32(4, bytes.length - 8, true);
  bytes.set(new TextEncoder().encode("WAVEfmt "), 8);
  data.setUint32(16, 16, true);
  data.setUint16(20, 1, true);
  data.setUint16(22, 1, true);
  data.setUint32(24, sampleRate, true);
  data.setUint32(28, sampleRate, true);
  data.setUint16(32, 1, true);
  data.setUint16(34, 8, true);
  bytes.set(new TextEncoder().encode("data"), 36);
  data.setUint32(40, samples, true);
  for (let index = 0; index < samples; index++) {
    bytes[44 + index] = Math.round(128 + 22 * Math.sin(2 * Math.PI * 440 * index / sampleRate) * (1 - index / samples));
  }
  return "data:audio/wav;base64," + btoa(String.fromCharCode(...bytes));
}

function drawExample(canvas: HTMLCanvasElement, step: number, title: string) {
  const context = canvas.getContext("2d");
  if (!context) return false;
  context.fillStyle = "#1d1b25";
  context.fillRect(0, 0, 520, 234);
  context.fillStyle = "#e7e4f6";
  context.font = "20px Segoe UI";
  context.fillText(title, 36, 48);
  context.font = "14px Segoe UI";
  ["Plan", "Approval", "Review"].forEach((item, index) => {
    context.fillStyle = index <= step ? "#514596" : "#2d2a38";
    context.fillRect(36 + index * 151, 95, 130, 46);
    context.fillStyle = "#ffffff";
    context.fillText(item, 52 + index * 151, 122);
  });
  context.fillStyle = "#afabc1";
  context.fillText("OmniAgent · example workflow", 36, 188);
  return true;
}

function ExampleImage() {
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => { if (canvas.current) drawExample(canvas.current, 1, "Release review"); }, []);
  return <canvas ref={canvas} width={520} height={234} className="note-media-canvas" aria-label="Illustration of the release approval sequence" />;
}

function ExampleVideo() {
  const [clip, setClip] = useState("");
  const [error, setError] = useState("");
  useEffect(() => {
    let current = true;
    const canvas = document.createElement("canvas");
    canvas.width = 520;
    canvas.height = 234;
    if (!canvas.captureStream || !window.MediaRecorder || !drawExample(canvas, 0, "Reopen an interrupted attempt")) {
      setError("Video preview is unavailable on this desktop.");
      return;
    }
    const stream = canvas.captureStream(12);
    let recorder: MediaRecorder;
    try {
      recorder = new MediaRecorder(stream, { mimeType: "video/webm" });
    } catch {
      stream.getTracks().forEach((track) => track.stop());
      setError("The desktop webview does not support WebM recording.");
      return;
    }
    const chunks: Blob[] = [];
    const reader = new FileReader();
    let index = 0;
    const tick = window.setInterval(() => {
      index++;
      drawExample(canvas, index, "Reopen an interrupted attempt");
      if (index === 3 && recorder.state === "recording") recorder.stop();
    }, 380);
    recorder.addEventListener("dataavailable", (event) => chunks.push(event.data));
    recorder.addEventListener("error", () => {
      if (current) setError("The sample video could not be produced.");
    });
    recorder.addEventListener("stop", () => {
      window.clearInterval(tick);
      stream.getTracks().forEach((track) => track.stop());
      if (current) reader.readAsDataURL(new Blob(chunks, { type: recorder.mimeType }));
    });
    reader.onload = () => { if (current && typeof reader.result === "string") setClip(reader.result); };
    recorder.start();
    return () => {
      current = false;
      window.clearInterval(tick);
      if (recorder.state === "recording") recorder.stop();
      stream.getTracks().forEach((track) => track.stop());
    };
  }, []);
  return clip ? <video className="note-media-canvas" src={clip} controls aria-label="Example recovery walkthrough" />
    : <p role={error ? "alert" : "status"}>{error || "Preparing local preview…"}</p>;
}

export function AttachmentPreview({ attachment }: { attachment: NoteAttachment }) {
  switch (attachment.kind) {
    case "pdf": return <PdfPreview />;
    case "image": return <ExampleImage />;
    case "audio": return <audio src={audioSource()} controls aria-label="Example release audio cue" />;
    case "video": return <ExampleVideo />;
    default: return <p role="status">The attachment is missing from the selected checkout.</p>;
  }
}
