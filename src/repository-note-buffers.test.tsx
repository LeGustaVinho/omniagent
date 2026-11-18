// @vitest-environment jsdom
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useRepositoryNotes, type NoteRevision } from "@/repository-note-buffers";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  Channel: class {
    onmessage: (event: { checkoutId: string; relativePath: string }) => void = () => undefined;
  },
}));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const path = "notes/Release.md";
const checkoutId = "checkout";
const revision = (id: string): NoteRevision => ({
  id, checkoutId, relativePath: path, sha256: id + "-sha256", baseCommit: null,
});
const documentAt = (rev: string, markdown: string) => ({
  status: "document", document: { revision: revision(rev), markdown, fileIdentity: "file" },
});

it("shares one canonical buffer, refreshes clean content and preserves edits during an external conflict", async () => {
  let disk = documentAt("r1", "# Original\n");
  let token = 0;
  let invalidate: ((event: { checkoutId: string; relativePath: string }) => void) | undefined;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "repository_note_catalog": return [{ path, frontmatter: null }];
      case "list_repository_markdown_drafts": return [];
      case "open_repository_document": return disk;
      case "read_repository_markdown_draft": return null;
      case "watch_repository_note":
        invalidate = (args as { channel: { onmessage: typeof invalidate } }).channel.onmessage;
        return "watch";
      case "save_repository_markdown_draft": return { revision: "draft-" + ++token };
      case "save_repository_document":
        disk = documentAt("r4", (args as { markdown: string }).markdown);
        return { ...disk, status: "saved" };
      case "discard_repository_markdown_draft": return true;
      case "unwatch_repository_note": return;
      default: throw new Error(command);
    }
  });
  const { result, unmount } = renderHook(() => useRepositoryNotes(checkoutId));
  await act(async () => { expect(await result.current.load("notes/release.md")).toBe(path); result.current.watch(path); });
  expect(Object.keys(result.current.notes)).toEqual([path]);
  const notice = { checkoutId, relativePath: path };
  await act(async () => { disk = documentAt("r2", "# Updated cleanly\n"); invalidate?.(notice); });
  await waitFor(() => expect(result.current.notes[path]?.content).toBe("# Updated cleanly\n"));

  act(() => result.current.update(path, "# Local edit\n"));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("save_repository_markdown_draft", {
    checkoutId, baseRevision: "r2", expectedDraftRevision: null, markdown: "# Local edit\n",
  }));
  await act(async () => { disk = documentAt("r3", "# Updated while dirty\n"); invalidate?.(notice); });
  await waitFor(() => expect(result.current.notes[path]?.external).toBe("# Updated while dirty\n"));
  expect(result.current.notes[path]?.content).toBe("# Local edit\n");
  await act(async () => { expect(await result.current.save(path)).toBe(false); });
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "save_repository_document")).toBe(false);
  await act(async () => { await result.current.resolve(path, true); });
  await act(async () => { expect(await result.current.save(path)).toBe(true); });
  expect(invoke).toHaveBeenCalledWith("save_repository_document", {
    checkoutId, path, expectedRevision: "r3", markdown: "# Local edit\n",
  });
  expect(invoke).toHaveBeenCalledWith("discard_repository_markdown_draft", {
    checkoutId, path, expectedDraftRevision: "draft-" + token,
  });
  expect(result.current.notes[path]?.saved).toBe("# Local edit\n");
  unmount();
  expect(invoke).toHaveBeenCalledWith("unwatch_repository_note", { watchId: "watch" });
});

it("restores the original revision and unsaved content beside an external revision after reopening", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "repository_note_catalog": return [];
      case "list_repository_markdown_drafts": return [path];
      case "open_repository_document": return documentAt("r2", "# External");
      case "read_repository_markdown_draft": return { base: revision("r1"), revision: "draft", markdown: "# Recovered buffer" };
      case "read_repository_markdown": return { markdown: "\uFEFF# Original\r\n" };
      default: throw new Error(command);
    }
  });
  const { result } = renderHook(() => useRepositoryNotes(checkoutId));
  await act(async () => { await result.current.load(path); });
  expect(result.current.draftPaths).toEqual([path]);
  expect(invoke).toHaveBeenCalledWith("read_repository_markdown", { checkoutId, revisionId: "r1" });
  expect(result.current.notes[path]).toMatchObject({
    base: revision("r1"), content: "# Recovered buffer", external: "# External", saved: "# Original\n", draftRevision: "draft",
  });
  await act(async () => { expect(await result.current.save(path)).toBe(false); });
  const calls = vi.mocked(invoke).mock.calls.map(([command]) => command);
  expect(calls).not.toContain("save_repository_document");
  expect(calls).not.toContain("discard_repository_markdown_draft");
  expect(result.current.notes[path]?.incoming?.revision.id).toBe("r2");
  expect(result.current.notes[path]?.draftRevision).toBe("draft");
});
