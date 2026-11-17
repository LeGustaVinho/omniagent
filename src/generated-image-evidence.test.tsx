// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { CapabilityOutputEvidence, type GeneratedImageEvidence } from "@/generated-image-evidence";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

it("lazily previews the recorded run event and explains unavailable output routes", async () => {
  const scope = { projectId: "project", conversationId: "conversation", runId: "run" };
  const image: GeneratedImageEvidence = {
    eventSequence: "43", itemId: "image-item", tool: "imageGeneration", producingModel: null,
    status: "completed", mime: "image/png", byteLen: 68, sha256: "saved-digest", unavailableReason: null,
  };
  const missing: GeneratedImageEvidence = {
    ...image, eventSequence: "45", mime: null, byteLen: null, sha256: null, unavailableReason: "inline_image_unavailable",
  };
  const encoded = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jH1sAAAAASUVORK5CYII=";
  vi.mocked(invoke).mockResolvedValue(encoded);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}>
    <CapabilityOutputEvidence scope={scope} generatedImages={[image, missing]} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  expect(invoke).not.toHaveBeenCalled();

  await user.click(screen.getByText("Generated image · Event 45"));
  expect(screen.getByText("The provider did not return inline image bytes.")).toBeTruthy();
  expect(invoke).not.toHaveBeenCalled();

  await user.click(screen.getByText("Generated image · Event 43"));
  expect((await screen.findByRole("img", { name: "Image generated in event 43" })).getAttribute("src"))
    .toBe(`data:image/png;base64,${encoded}`);
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(invoke).toHaveBeenCalledWith("conversation_generated_image_evidence", { ...scope, eventSequence: "43" });
  expect(screen.getByText("Generated audio · Evidence capture unavailable")).toBeTruthy();
  expect(screen.getByText("Browser and host actions · Evidence capture unavailable")).toBeTruthy();
});

it("qualifies only the exact decoded provider output after an explicit action", async () => {
  const scope = { projectId: "project", conversationId: "conversation", runId: "run" };
  const image: GeneratedImageEvidence = {
    eventSequence: "43", itemId: "image-item", tool: "imageGeneration", producingModel: null,
    status: "completed", mime: "image/png", byteLen: 68, sha256: "saved-digest", unavailableReason: null,
  };
  vi.mocked(invoke).mockResolvedValueOnce("image-bytes").mockResolvedValueOnce({ id: "saved-image-proof", checkedAt: "checked" });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><CapabilityOutputEvidence scope={scope} generatedImages={[image]} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByText("Generated image · Event 43"));
  const preview = await screen.findByRole("img", { name: "Image generated in event 43" });
  expect(screen.queryByRole("button", { name: "Qualify image generation for this profile" })).toBeNull();
  fireEvent.load(preview);
  expect(invoke).toHaveBeenCalledTimes(1);
  await user.click(screen.getByRole("button", { name: "Qualify image generation for this profile" }));
  expect(invoke).toHaveBeenCalledWith("qualify_conversation_generated_image", { ...scope, eventSequence: "43" });
  expect(await screen.findByText("saved-image-proof")).toBeTruthy();
});
