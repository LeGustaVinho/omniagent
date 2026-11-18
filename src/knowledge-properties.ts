import { invoke } from "@tauri-apps/api/core";
import { useQueries, useQuery } from "@tanstack/react-query";
import { getProperties, noteFrontmatter } from "@/knowledge-document";
import type { KnowledgeSearchHit } from "@/knowledge-navigation";

export type NoteYamlProperty = { name: string; value: string; line: number; editable: boolean; filterable: boolean };
export type NotePropertyFilter = Pick<NoteYamlProperty, "name" | "value">;
type PropertyHit = KnowledgeSearchHit & { value: string };

function localProperties(markdown: string): NoteYamlProperty[] {
  return getProperties(markdown).map(({ key, value, line }) => ({ name: key, value, line, editable: false, filterable: false }));
}

export function useKnowledgeProperties(sources: readonly string[]) {
  const frontmatters = [...new Set(sources.flatMap((source) => noteFrontmatter(source) ?? []))];
  const queries = useQueries({ queries: frontmatters.map((markdown) => ({
    queryKey: ["markdown-properties", markdown],
    queryFn: () => invoke<NoteYamlProperty[]>("describe_note_properties", { markdown }),
    staleTime: Infinity,
  })) });
  const parsed = new Map(frontmatters.map((markdown, index) => [markdown, queries[index]]));
  return (source: string) => {
    const frontmatter = noteFrontmatter(source);
    const query = frontmatter ? parsed.get(frontmatter) : undefined;
    return {
      rows: Array.isArray(query?.data) ? query.data : frontmatter ? localProperties(frontmatter) : [],
      error: query?.error,
      pending: Boolean(query?.isPending),
    };
  };
}

export function useKnowledgePropertySearch({ checkoutId, filter, drafts, indexVersion }: {
  checkoutId: string; filter?: NotePropertyFilter;
  drafts: Record<string, string | null>; indexVersion?: number;
}) {
  const result = useQuery({
    queryKey: ["repository-note-property-search", checkoutId, filter, indexVersion, drafts],
    enabled: Boolean(filter && indexVersion),
    queryFn: () => invoke<PropertyHit[]>("search_repository_note_properties", {
      checkoutId, key: filter?.name, value: filter?.value, drafts,
    }),
  });
  return {
    pending: Boolean(filter) && (!indexVersion || result.isFetching),
    error: result.error,
    hits: (Array.isArray(result.data) ? result.data : []).map(({ value, ...hit }) => ({ ...hit, snippet: `${filter?.name ?? ""}: ${value}` })),
  };
}
