import { invoke } from "@tauri-apps/api/core";
import { useQueries, useQuery } from "@tanstack/react-query";
import { fileName, type ExampleFile } from "@/ide-knowledge-data";
import { getProperties, noteFrontmatter } from "@/knowledge-document";
import type { KnowledgeSearchHit } from "@/knowledge-navigation";

export type NoteYamlProperty = { name: string; value: string; line: number; editable: boolean; filterable: boolean };
export type NotePropertyFilter = Pick<NoteYamlProperty, "name" | "value">;
type PropertyHit = KnowledgeSearchHit & { value: string };

function localProperties(markdown: string, simulated: boolean): NoteYamlProperty[] {
  const rows = getProperties(markdown);
  return rows.map(({ key, value, line }) => {
    const simple = simulated && !/^[\[\]{'"!&*|>]/.test(value) && rows.filter((row) => row.key === key).length === 1;
    return { name: key, value, line, editable: simple, filterable: simple };
  });
}

export function useKnowledgeProperties(sources: readonly string[], simulated: boolean) {
  const frontmatters = simulated ? [] : [...new Set(sources.flatMap((source) => noteFrontmatter(source) ?? []))];
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
      rows: Array.isArray(query?.data) ? query.data : frontmatter ? localProperties(frontmatter, simulated) : [],
      error: query?.error,
      pending: Boolean(query?.isPending),
    };
  };
}

export function useKnowledgePropertySearch({ checkoutId, simulated, filter, catalog, drafts, indexVersion }: {
  checkoutId: string; simulated: boolean; filter?: NotePropertyFilter; catalog: readonly ExampleFile[];
  drafts: Record<string, string | null>; indexVersion?: number;
}) {
  const result = useQuery({
    queryKey: ["repository-note-property-search", checkoutId, filter, indexVersion, drafts],
    enabled: !simulated && Boolean(filter && indexVersion),
    queryFn: () => invoke<PropertyHit[]>("search_repository_note_properties", {
      checkoutId, key: filter?.name, value: filter?.value, drafts,
    }),
  });
  const hits = simulated ? filter && catalog.flatMap(({ path, content }) => {
    const property = localProperties(content, true).find(({ name, value, filterable }) => filterable && name === filter.name && value === filter.value);
    return property ? [{ path, title: fileName(path), line: property.line, value: property.value }] : [];
  }) : result.data;
  return {
    pending: !simulated && Boolean(filter) && (!indexVersion || result.isFetching),
    error: result.error,
    hits: (Array.isArray(hits) ? hits : []).map(({ value, ...hit }) => ({ ...hit, snippet: `${filter?.name ?? ""}: ${value}` })),
  };
}
