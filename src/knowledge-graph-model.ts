import { fileName, type ExampleFile } from "@/ide-knowledge-data";
import { resolveWiki, wikiLinks } from "@/knowledge-document";

export interface NoteGraphNode {
  readonly id?: number;
  readonly path: string;
  readonly title: string;
  readonly tags: readonly string[];
}

export interface NoteGraphEdge {
  readonly sourcePath: string;
  readonly targetPath: string;
}

export interface NoteGraphProjection {
  readonly notes: readonly NoteGraphNode[];
  readonly edges: readonly NoteGraphEdge[];
}

export function exampleNoteGraph(files: readonly ExampleFile[]): NoteGraphProjection {
  return {
    notes: files.map(({ path }) => ({ path, title: fileName(path).replace(/\.md$/i, ""), tags: [] })),
    edges: files.flatMap(({ path, content }) => wikiLinks(content).flatMap((link) => {
      const resolved = resolveWiki(link, path, files);
      const target = resolved.length === 1 ? resolved[0] : undefined;
      return target ? [{ sourcePath: path, targetPath: target.path }] : [];
    })),
  };
}

export function indexNoteGraph({ notes, edges }: NoteGraphProjection) {
  const byPath = new Map(notes.map((note) => [note.path, note]));
  const neighbors = new Map<string, Set<string>>();
  const unique = new Map<string, NoteGraphEdge>();
  const tags = new Map<string, string>();
  for (const note of byPath.values()) {
    neighbors.set(note.path, new Set());
    for (const tag of note.tags) if (!tags.has(tag.toLowerCase())) tags.set(tag.toLowerCase(), tag);
  }
  for (const edge of edges) {
    const { sourcePath, targetPath } = edge;
    const sources = neighbors.get(sourcePath);
    const targets = neighbors.get(targetPath);
    if (!sources || !targets || sourcePath === targetPath) continue;
    sources.add(targetPath);
    targets.add(sourcePath);
    unique.set(JSON.stringify([sourcePath, targetPath]), edge);
  }
  return { byPath, neighbors, edges: [...unique.values()], tags: [...tags.values()].sort((a, b) => a.localeCompare(b)) };
}

export function selectNoteGraph(
  graph: ReturnType<typeof indexNoteGraph>,
  { active, depth, search, tag }: { active?: string; depth: number; search: string; tag: string },
) {
  const anchor = active && graph.byPath.has(active) ? active : undefined;
  const distance = new Map<string, number>();
  if (anchor) {
    let frontier = [anchor];
    distance.set(anchor, 0);
    for (let level = 1; level <= depth && frontier.length; level++) {
      const next: string[] = [];
      for (const path of frontier) {
        for (const neighbor of graph.neighbors.get(path) ?? []) {
          if (distance.has(neighbor)) continue;
          distance.set(neighbor, level);
          next.push(neighbor);
        }
      }
      frontier = next;
    }
  }
  const query = search.trim().toLowerCase();
  const selectedTag = tag.toLowerCase();
  const notes = [...graph.byPath.values()].filter((note) => {
    if (anchor) {
      if (note.path === anchor) return true;
      if (!distance.has(note.path)) return false;
    }
    const matchesSearch = !query || [note.path, note.title, ...note.tags].some((value) => value.toLowerCase().includes(query));
    const matchesTag = !selectedTag || note.tags.some((value) => value.toLowerCase() === selectedTag);
    return matchesSearch && matchesTag;
  });
  const visible = new Set(notes.map(({ path }) => path));
  const edges = graph.edges.filter(({ sourcePath, targetPath }) => visible.has(sourcePath) && visible.has(targetPath));
  return { notes, edges, distance };
}
