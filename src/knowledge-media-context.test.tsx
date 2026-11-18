// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { downloadArtifact } from "@/download-artifact";
import { renderNoteDiagram } from "@/knowledge-diagram";
import { downloadNoteDiagramPng } from "@/knowledge-media";
import { NoteReading } from "@/knowledge-reading";
import { SafeAttachmentMedia } from "@/repository-note-media";

vi.mock("@/download-artifact", () => ({ downloadArtifact: vi.fn() }));
vi.mock("@/knowledge-diagram", () => ({ renderNoteDiagram: vi.fn() }));
vi.mock("pdfjs-dist", () => ({
  GlobalWorkerOptions: {},
  getDocument: () => ({
    destroy: async () => {},
    promise: Promise.resolve({
      numPages: 2,
      getPage: async (page: number) => ({
        getViewport: () => ({ width: 600, height: 800 }),
        render: () => ({ promise: Promise.resolve(), cancel: () => {} }),
        getTextContent: async () => ({ items: page === 1 ? [
          { str: "First line", hasEOL: true }, { str: "Second", hasEOL: false }, { str: " line", hasEOL: false },
        ] : [] }),
      }),
    }),
  }),
}));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.clearAllMocks();
});

it("copies the displayed PDF page as a Markdown quote and omits the action on image-only pages", async () => {
  const user = userEvent.setup();
  const clipboard = vi.spyOn(navigator.clipboard, "writeText");
  render(<SafeAttachmentMedia base64="JVBERg==" mimeType="application/pdf" title="Checklist" relativePath="notes/evidence.pdf" />);
  const first = await screen.findByLabelText("Checklist page 1");
  await waitFor(() => expect(first.tabIndex).toBe(0));
  fireEvent.contextMenu(first);
  await user.click(await screen.findByRole("menuitem", { name: "Copy page as quote" }));
  expect(clipboard).toHaveBeenCalledWith("> First line\n> Second line\n\n— notes/evidence.pdf · Page 1");

  await user.click(screen.getByRole("button", { name: "Next page" }));
  const second = await screen.findByLabelText("Checklist page 2");
  await waitFor(() => expect(second.hidden).toBe(false));
  fireEvent.contextMenu(second);
  expect(screen.queryByRole("menuitem", { name: "Copy page as quote" })).toBeNull();
});

it("exposes a fully rendered diagram image and exports that same image to a PNG", async () => {
  vi.mocked(renderNoteDiagram).mockResolvedValue("data:image/svg+xml," + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg"/>'));
  const content = "\`\`\`mermaid\nflowchart LR\nA --> B\n\`\`\`\n";
  const path = "notes/diagram.md";
  const contextActions = vi.fn(() => []);
  render(<NoteReading path={path} content={content} files={[{ path, content }]} onLink={() => {}}
    contextActions={contextActions} />);
  const image = await screen.findByRole<HTMLImageElement>("img", { name: "Mermaid diagram from the note" });
  fireEvent.contextMenu(image);
  expect(contextActions.mock.calls).toHaveLength(1);
  expect(contextActions).toHaveBeenLastCalledWith(expect.objectContaining({ diagramImage: undefined }));
  Object.defineProperties(image, {
    complete: { value: true }, naturalWidth: { value: 300 }, naturalHeight: { value: 200 },
  });
  fireEvent.contextMenu(image);
  expect(contextActions).toHaveBeenLastCalledWith(expect.objectContaining({ diagramImage: image }));

  const drawImage = vi.fn();
  const png = new Blob(["png bytes"], { type: "image/png" });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ drawImage } as unknown as CanvasRenderingContext2D);
  vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation((callback) => { callback(png); });
  await downloadNoteDiagramPng(image, "diagram.png");
  expect(drawImage).toHaveBeenCalledWith(image, 0, 0, 600, 400);
  expect(downloadArtifact).toHaveBeenCalledWith("diagram.png", png);
});
