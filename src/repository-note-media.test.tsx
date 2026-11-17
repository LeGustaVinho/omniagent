// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { RepositoryNoteMedia } from "@/repository-note-media";
import { decodeArtifactBytes, downloadArtifact } from "@/download-artifact";

const pdf = vi.hoisted(() => ({ getDocument: vi.fn(), GlobalWorkerOptions: { workerSrc: "" } }));
vi.mock("pdfjs-dist", () => pdf);
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/download-artifact", async (original) => ({
  ...await original<typeof import("@/download-artifact")>(), downloadArtifact: vi.fn(),
}));

const checkoutId = "notes-checkout";
const invokeAttachment = vi.mocked(invoke);
const clients: QueryClient[] = [];

afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  vi.resetAllMocks();
  pdf.GlobalWorkerOptions.workerSrc = "";
});

function showAttachment(paths: string[], title: string, allowSaveCopy = false) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  clients.push(client);
  return render(
    <QueryClientProvider client={client}>
      <RepositoryNoteMedia checkoutId={checkoutId} paths={paths} title={title} allowSaveCopy={allowSaveCopy} />
    </QueryClientProvider>,
  );
}

const media = [
  {
    path: "media/review.png", mimeType: "image/png", element: "IMG", title: "Release review diagram",
    contentBase64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/w8AAgMBAp8BFDsAAAAASUVORK5CYII=",
  },
  {
    path: "media/cue.wav", mimeType: "audio/x-wav", element: "AUDIO", title: "Release audio cue",
    contentBase64: "UklGRiYAAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YQIAAACAgA==",
  },
  {
    path: "media/walkthrough.webm", mimeType: "video/webm", element: "VIDEO", title: "Release walkthrough",
    contentBase64: "GkXfo59ChoEBQveBAULygQRC84EIQoKEd2VibUKHgQJChYEC",
  },
];

it.each(media)("renders $mimeType with the label '$title' after the first candidate misses", async (file) => {
  const { path, mimeType, element, title, contentBase64 } = file;
  const noteRelative = "notes/" + path;
  invokeAttachment.mockResolvedValueOnce(null).mockResolvedValueOnce({ relativePath: path, mimeType, contentBase64 });

  showAttachment([noteRelative, path], title);
  expect(screen.getByRole("status").textContent).toBe(`Loading attachment · ${title}`);
  const player = element === "IMG"
    ? await screen.findByRole("img", { name: title })
    : await screen.findByLabelText(title);
  expect(player.tagName).toBe(element);
  expect(player.getAttribute("src")).toBe(`data:${mimeType};base64,${contentBase64}`);
  if (element === "IMG") {
    expect(player.getAttribute("alt")).toBe(title);
  } else {
    expect(player.hasAttribute("controls")).toBe(true);
    expect(player.getAttribute("preload")).toBe("metadata");
  }
  expect(invokeAttachment.mock.calls).toEqual([
    ["read_repository_attachment", { checkoutId, path: noteRelative }],
    ["read_repository_attachment", { checkoutId, path }],
  ]);
});

it("saves the resolved attachment's original bytes and file name from its preview", async () => {
  const { path, mimeType, contentBase64 } = media[0]!;
  invokeAttachment.mockResolvedValueOnce(null).mockResolvedValueOnce({ relativePath: path, mimeType, contentBase64 });
  showAttachment(["notes/missing.png", path], "Diagram alias", true);
  fireEvent.click(await screen.findByRole("button", { name: "Save a copy" }));
  const [name, blob] = vi.mocked(downloadArtifact).mock.calls[0]!;
  expect(name).toBe("review.png");
  expect(blob.type).toBe(mimeType);
  const bytes = await new Promise<Uint8Array>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(new Uint8Array(reader.result as ArrayBuffer));
    reader.onerror = reject;
    reader.readAsArrayBuffer(blob);
  });
  expect(bytes).toEqual(decodeArtifactBytes(contentBase64));
});

it("renders the selected PDF page to a bounded canvas with labeled navigation", async () => {
  const path = "evidence/readiness.pdf";
  const title = "Release readiness";
  invokeAttachment.mockResolvedValueOnce({
    relativePath: path, mimeType: "application/pdf", contentBase64: btoa("%PDF-1.7\n"),
  });
  const getViewport = vi.fn(({ scale }: { scale: number }) => ({ width: 2_800 * scale, height: 1_400 * scale }));
  const renderPage = vi.fn(() => ({ promise: Promise.resolve(), cancel: vi.fn() }));
  const getTextContent = vi.fn(async () => ({ items: [{ str: "Release checklist", hasEOL: true }] }));
  const getPage = vi.fn(async () => ({ getViewport, render: renderPage, getTextContent }));
  const destroy = vi.fn(async () => undefined);
  pdf.getDocument.mockImplementation(() => ({ promise: Promise.resolve({ numPages: 2, getPage }), destroy }));

  const { unmount } = showAttachment([path], title);
  const canvas = await screen.findByLabelText<HTMLCanvasElement>(`${title} page 1`);
  await waitFor(() => expect(canvas.hidden).toBe(false));
  expect(getTextContent).toHaveBeenCalledTimes(1);
  expect(pdf.GlobalWorkerOptions.workerSrc).toContain("pdf.worker.min.mjs");
  const parameters = pdf.getDocument.mock.lastCall?.[0];
  expect(parameters).toMatchObject({ enableXfa: false, useSystemFonts: true });
  expect([...parameters.data]).toEqual([...new TextEncoder().encode("%PDF-1.7\n")]);
  expect(getViewport).toHaveBeenCalledWith({ scale: 0.5 });
  expect([canvas.width, canvas.height]).toEqual([1_400, 700]);
  expect(renderPage).toHaveBeenCalledWith({
    canvas, viewport: { width: 1_400, height: 700 }, background: "#ffffff",
  });
  const previous = screen.getByRole<HTMLButtonElement>("button", { name: "Previous page" });
  const next = screen.getByRole<HTMLButtonElement>("button", { name: "Next page" });
  expect(screen.getByText("Page 1 of 2")).toBeTruthy();
  expect([previous.disabled, next.disabled]).toEqual([true, false]);
  fireEvent.click(next);
  await waitFor(() => expect(getPage).toHaveBeenLastCalledWith(2));
  expect(screen.getByLabelText(`${title} page 2`)).toBe(canvas);
  expect(screen.getByText("Page 2 of 2")).toBeTruthy();
  expect([previous.disabled, next.disabled]).toEqual([false, true]);
  fireEvent.click(previous);
  await waitFor(() => expect(getPage).toHaveBeenLastCalledWith(1));
  unmount();
  expect(destroy).toHaveBeenCalledTimes(3);
});

it("shows a labeled placeholder for an attachment missing from the selected checkout", async () => {
  invokeAttachment.mockResolvedValueOnce(null);
  const { container } = showAttachment(["notes/archive.png"], "Archived image");
  expect(await screen.findByText("Attachment unavailable · Archived image")).toBeTruthy();
  expect(container.querySelector("img, audio, video, canvas")).toBeNull();
  expect(invokeAttachment).toHaveBeenCalledExactlyOnceWith("read_repository_attachment", {
    checkoutId, path: "notes/archive.png",
  });
});

it("shows a labeled placeholder for bytes with an unsupported media type", async () => {
  invokeAttachment.mockResolvedValueOnce({
    relativePath: "notes/diagram.png", mimeType: "application/octet-stream", contentBase64: btoa("not a PNG"),
  });
  const { container } = showAttachment(["notes/diagram.png"], "Damaged diagram");
  expect(await screen.findByText("Attachment preview unavailable · Damaged diagram")).toBeTruthy();
  expect(container.querySelector("img, audio, video, canvas, iframe")).toBeNull();
});

it("shows a labeled placeholder when the browser cannot decode a recognized image", async () => {
  invokeAttachment.mockResolvedValueOnce({
    relativePath: "notes/corrupt.png", mimeType: "image/png", contentBase64: "iVBORw0KGgo=",
  });
  showAttachment(["notes/corrupt.png"], "Corrupt chart");
  const image = await screen.findByRole("img", { name: "Corrupt chart" });
  fireEvent.error(image);
  expect(await screen.findByText("Attachment preview unavailable · Corrupt chart")).toBeTruthy();
  expect(screen.queryByRole("img", { name: "Corrupt chart" })).toBeNull();
});

it("reports an invalid PDF and leaves its canvas hidden", async () => {
  invokeAttachment.mockResolvedValueOnce({
    relativePath: "notes/damaged.pdf", mimeType: "application/pdf", contentBase64: btoa("%PDF-invalid"),
  });
  pdf.getDocument.mockImplementationOnce(() => ({
    promise: Promise.reject(new Error("Invalid PDF structure")), destroy: vi.fn(async () => undefined),
  }));
  showAttachment(["notes/damaged.pdf"], "Damaged PDF");
  await waitFor(() => expect(screen.getByRole("status").textContent).toBe("This PDF could not be rendered."));
  expect(screen.getByLabelText<HTMLCanvasElement>("Damaged PDF page 1").hidden).toBe(true);
  expect(screen.queryByRole("button", { name: "Next page" })).toBeNull();
});
