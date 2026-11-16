import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import type { Completion } from "@codemirror/autocomplete";
import { notePreviewLinks, type NoteTarget, type NoteLocation } from "@/knowledge-document";
import type { ExampleFile } from "@/ide-knowledge-data";

export type IndexedLinkLocation = { path: string; anchor: string | null; line: number | null };
export type IndexedLinkResolution = { status: "resolved" | "missing_anchor"; location: IndexedLinkLocation }
  | { status: "ambiguous"; candidates: IndexedLinkLocation[] }
  | { status: "attachment"; paths: string[] }
  | { status: "missing" | "invalid" };
export type IndexedNoteDetails = {
  headings: { label: string; level: number; line: number }[];
};
export type IndexedNoteRelationships = {
  outgoing: {
    link: { target: string; alias: string | null; anchor: string | null; line: number; syntax: "markdown" | "wiki"; embed: boolean };
    resolution: IndexedLinkResolution;
  }[];
  backlinks: { path: string; title: string; line: number; label: string | null }[];
};

function linkQuery(link: NoteTarget, path: string) {
  return { sourcePath: link.sourcePath ?? path, target: link.target, anchor: link.anchor ?? null };
}
type LinkQuery = ReturnType<typeof linkQuery>;
const key = (query: LinkQuery) => JSON.stringify([query.sourcePath, query.target, query.anchor]);

export async function resolveRepositoryLink(checkoutId: string, link: NoteTarget, path: string) {
  const results = await invoke<IndexedLinkResolution[]>("resolve_repository_note_links", { checkoutId, queries: [linkQuery(link, path)] });
  return results[0];
}

export async function suggestRepositoryLinks(checkoutId: string, sourcePath: string, prefix: string): Promise<Completion[]> {
  type Suggestion = { path: string; target: string; label: string };
  const results = await invoke<Suggestion[]>("suggest_repository_note_links", { checkoutId, sourcePath, prefix, limit: 40 });
  return results.map(({ path, target, label }) => ({ label, detail: path, apply: target, type: "text" }));
}

export function useRepositoryLinkPreviews(checkoutId: string, files: readonly ExampleFile[], version: number, enabled: boolean) {
  const queries = new Map<string, LinkQuery>();
  if (enabled) for (const { path, content } of files) {
    for (const link of notePreviewLinks(content, path)) {
      const query = linkQuery(link, path);
      queries.set(key(query), query);
    }
  }
  const batched = [...queries.values()];
  return useQuery({
    queryKey: ["repository-note-link-previews", checkoutId, version, batched],
    enabled: enabled && Boolean(batched.length) && Boolean(version),
    queryFn: async () => {
      const responses = await invoke<IndexedLinkResolution[]>("resolve_repository_note_links", { checkoutId, queries: batched });
      return new Map(responses.map((response, i) => [key(batched[i]!), response]));
    },
  });
}

export function linkPreviewValue(index: Map<string, IndexedLinkResolution> | undefined, link: NoteTarget, path: string) {
  return index?.get(key(linkQuery(link, path)));
}

export function asNoteLocation(location: IndexedLinkLocation): NoteLocation {
  return { path: location.path, anchor: location.anchor ?? undefined };
}
